//! Which order to emit a set in, so the prefix cache still holds most of it.
//!
//! See `docs/notes/server-prefix-cache.md`.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, MutexGuard, OnceLock};

use serde::{Deserialize, Serialize};

use crate::store::Store;

/// Every session's layout, read from `store` the first time that session asks.
///
/// # Invariant
///
/// One layout per session: a provider caches the prefix of a conversation, so what
/// another session emitted is not warm for this one, and a chain only this session
/// can use must not be chosen for it.
fn shared(store: &Store, session: &str) -> MutexGuard<'static, HashMap<String, Layout>> {
    static SHARED: OnceLock<Mutex<HashMap<String, Layout>>> = OnceLock::new();
    let mut layouts = SHARED
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if !layouts.contains_key(session) {
        let layout = match store.prefix_layout(session) {
            Ok(Some(mut layout)) => {
                layout.reindex();
                layout
            }
            Ok(None) => Layout::new(),
            Err(e) => {
                tracing::warn!(error = %e, session = %session, "the stored prefix layout cannot be read; starting empty");
                Layout::new()
            }
        };
        layouts.insert(session.to_string(), layout);
    }
    drop(layouts);
    SHARED
        .get()
        .expect("just installed")
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// The digest that says "the same text as last time", for a caller's content.
pub fn digest(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

/// The cheapest order for `wanted`, learnt across what this session has emitted.
pub fn plan(
    store: &Store,
    session: &str,
    purpose: &str,
    wanted: &[(&str, usize, u64)],
) -> Vec<String> {
    let mut layouts = shared(store, session);
    let Some(layout) = layouts.get_mut(session) else {
        return Vec::new();
    };
    let order = layout.plan(purpose, wanted);
    if let Err(e) = store.save_prefix_layout(session, layout) {
        tracing::warn!(error = %e, session = %session, "the learnt prefix layout could not be written");
    }
    order
}

/// What a byte sent again costs against a byte the cache already holds.
///
/// liana's `cache_miss_cost_scale`.
const MISS_SCALE: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct NodeId(usize);

/// One thing that can be emitted: a table row, an indexed reference, a message.
///
/// # Invariant
///
/// The identity is the text, not the name: edited content is a different item, or
/// the cache would claim a prefix it cannot serve.
#[derive(Debug, Serialize, Deserialize)]
pub struct Item {
    /// Which caller emitted it: two callers prune their own, never each other's.
    pub purpose: String,
    /// The name the caller knows it by, which may come to name other text later.
    pub key: String,
    /// A digest of the bytes, which is what says "the same text as last time".
    pub content: u64,
    /// One node per order this item has ended.
    nodes: Vec<NodeId>,
    /// The bytes sending it costs the provider.
    size: usize,
}

#[derive(Debug, Serialize, Deserialize)]
struct Node {
    item: usize,
    parent: Option<NodeId>,
    children: Vec<NodeId>,
    /// Every ancestor's size and its own: what a chain ending here carries.
    size: usize,
}

/// Every order emitted so far, as a forest of chains, with what each one costs.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Layout {
    items: Vec<Item>,
    /// `key` and the digest of its text, to the item that text is. Derived from
    /// `items`, so it is rebuilt rather than stored.
    #[serde(skip)]
    index: HashMap<(String, String, u64), usize>,
    nodes: Vec<Node>,
}

impl Layout {
    pub fn new() -> Self {
        Self::default()
    }

    /// Rebuilds the lookup from `items`, for a layout that came off disk.
    pub fn reindex(&mut self) {
        self.index.clear();
        for (id, item) in self.items.iter().enumerate() {
            self.index
                .insert((item.purpose.clone(), item.key.clone(), item.content), id);
        }
    }

    /// The cheapest chain, then whatever is new. See the note.
    pub fn plan(&mut self, purpose: &str, wanted: &[(&str, usize, u64)]) -> Vec<String> {
        let ids = self.ids(purpose, wanted);
        if ids.is_empty() {
            return Vec::new();
        }
        // What sending all of it with nothing cached costs: the floor to beat.
        let mut best: usize = ids.iter().map(|&id| self.items[id].size).sum::<usize>() * MISS_SCALE;
        let mut chosen: Option<NodeId> = None;
        for (node, cost) in self.cold_chains(&ids).into_iter().enumerate() {
            let Some(cost) = cost else { continue };
            if cost < best {
                best = cost;
                chosen = Some(NodeId(node));
            }
        }
        let mut warm = HashSet::new();
        let mut walk = chosen;
        while let Some(node) = walk {
            warm.insert(self.nodes[node.0].item);
            walk = self.nodes[node.0].parent;
        }
        let mut tail = chosen;
        for &id in &ids {
            if !warm.contains(&id) {
                tail = Some(self.add(id, tail));
            }
        }
        let order = self.order(tail);
        // After the plan, never before: forgetting first would renumber the items
        // these ids name.
        self.forget_unwanted(purpose, &ids);
        order
    }

    /// Each chain's cost to send, and `None` for a chain that cannot be sent now.
    ///
    /// # Invariant
    ///
    /// A chain carrying an item outside `wanted` is not a candidate, whatever it
    /// would cost: a row no longer installed must not come back because it was
    /// emitted once.
    fn cold_chains(&self, ids: &[usize]) -> Vec<Option<usize>> {
        let mut allowed = vec![true; self.nodes.len()];
        for id in 0..self.items.len() {
            if ids.contains(&id) {
                continue;
            }
            for node in self.subtree(id) {
                allowed[node] = false;
            }
        }
        let mut costs: Vec<Option<usize>> = self
            .nodes
            .iter()
            .enumerate()
            .map(|(node, n)| allowed[node].then_some(n.size))
            .collect();
        for &id in ids {
            let carried = self.subtree_marks(id);
            let size = self.items[id].size;
            for (node, cost) in costs.iter_mut().enumerate() {
                if let Some(cost) = cost {
                    if !carried[node] {
                        *cost += size * MISS_SCALE;
                    }
                }
            }
        }
        costs
    }

    /// Which chains already carry `item`: its own nodes, and every chain below them.
    fn subtree_marks(&self, item: usize) -> Vec<bool> {
        let mut carried = vec![false; self.nodes.len()];
        for node in self.subtree(item) {
            carried[node] = true;
        }
        carried
    }

    fn subtree(&self, item: usize) -> Vec<usize> {
        let mut seen = Vec::new();
        let mut stack = self.items[item].nodes.clone();
        while let Some(node) = stack.pop() {
            seen.push(node.0);
            stack.extend(self.nodes[node.0].children.iter().copied());
        }
        seen
    }

    /// The item ids `wanted` names, in its order, declaring what is new.
    fn ids(&mut self, purpose: &str, wanted: &[(&str, usize, u64)]) -> Vec<usize> {
        let mut ids: Vec<usize> = Vec::with_capacity(wanted.len());
        let mut restated = false;
        for &(key, size, content) in wanted {
            let id = match self
                .index
                .get(&(purpose.to_string(), key.to_string(), content))
            {
                Some(&id) => {
                    if self.items[id].size != size {
                        self.items[id].size = size;
                        restated = true;
                    }
                    id
                }
                None => {
                    let id = self.items.len();
                    self.items.push(Item {
                        purpose: purpose.to_string(),
                        key: key.to_string(),
                        content,
                        nodes: Vec::new(),
                        size,
                    });
                    self.index
                        .insert((purpose.to_string(), key.to_string(), content), id);
                    id
                }
            };
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        if restated {
            self.recost();
        }
        ids
    }

    /// Drops every item whose text is not in `ids`, and the chains through it.
    ///
    /// # Invariant
    ///
    /// What stays can only be what was just asked for: a chain left through an item
    /// nobody wants is a chain no future plan could choose, so keeping it would make
    /// the layout grow without ever paying for itself.
    fn forget_unwanted(&mut self, purpose: &str, ids: &[usize]) {
        let live = (0..self.items.len())
            .filter(|item| self.items[*item].purpose == purpose)
            .count();
        if live == ids.len() {
            return;
        }
        let mut doomed = vec![false; self.nodes.len()];
        for item in 0..self.items.len() {
            let mine = self.items[item].purpose == purpose;
            if mine && !ids.contains(&item) {
                for node in self.subtree(item) {
                    doomed[node] = true;
                }
            }
        }
        let mut remap: Vec<Option<usize>> = vec![None; self.nodes.len()];
        let mut next = 0;
        for (node, kept) in remap.iter_mut().enumerate() {
            if !doomed[node] {
                *kept = Some(next);
                next += 1;
            }
        }
        let nodes = std::mem::take(&mut self.nodes);
        self.nodes = nodes
            .into_iter()
            .enumerate()
            .filter(|(node, _)| !doomed[*node])
            .map(|(_, node)| Node {
                item: node.item,
                parent: node.parent.and_then(|parent| remap[parent.0]).map(NodeId),
                children: node
                    .children
                    .into_iter()
                    .filter_map(|child| remap[child.0].map(NodeId))
                    .collect(),
                size: node.size,
            })
            .collect();
        let kept: Vec<usize> = (0..self.items.len())
            .filter(|id| self.items[*id].purpose != purpose || ids.contains(id))
            .collect();
        let reindex: Vec<Option<usize>> = (0..self.items.len())
            .map(|id| kept.iter().position(|kept| *kept == id))
            .collect();
        let items = std::mem::take(&mut self.items);
        self.items = items
            .into_iter()
            .enumerate()
            .filter(|(id, _)| reindex[*id].is_some())
            .map(|(_, mut item)| {
                item.nodes = item
                    .nodes
                    .into_iter()
                    .filter_map(|node| remap[node.0].map(NodeId))
                    .collect();
                item
            })
            .collect();
        for node in &mut self.nodes {
            node.item = reindex[node.item].expect("a kept node's item was kept");
        }
        self.reindex();
    }

    /// Re-derives every chain's size after one item's size changed.
    ///
    /// # Invariant
    ///
    /// A parent is always declared before its children, so one pass in node order
    /// reads sizes it has already corrected.
    fn recost(&mut self) {
        for node in 0..self.nodes.len() {
            let item = self.nodes[node].item;
            let above = self.nodes[node]
                .parent
                .map_or(0, |parent| self.nodes[parent.0].size);
            self.nodes[node].size = above + self.items[item].size;
        }
    }

    fn add(&mut self, item: usize, parent: Option<NodeId>) -> NodeId {
        let id = NodeId(self.nodes.len());
        let carried = match parent {
            Some(parent) => {
                self.nodes[parent.0].children.push(id);
                self.nodes[parent.0].size
            }
            None => 0,
        };
        self.nodes.push(Node {
            item,
            parent,
            children: Vec::new(),
            size: carried + self.items[item].size,
        });
        self.items[item].nodes.push(id);
        id
    }

    fn order(&self, tail: Option<NodeId>) -> Vec<String> {
        let mut keys = Vec::new();
        let mut walk = tail;
        while let Some(node) = walk {
            keys.push(self.items[self.nodes[node.0].item].key.clone());
            walk = self.nodes[node.0].parent;
        }
        keys.reverse();
        keys
    }

    #[cfg(test)]
    fn chain_count(&self) -> usize {
        self.nodes.len()
    }

    /// What a fresh plan of `wanted` would choose, without teaching the layout.
    #[cfg(test)]
    fn chain_order(&self, purpose: &str, wanted: &[(&str, usize, u64)]) -> Vec<String> {
        let ids: Vec<usize> = wanted
            .iter()
            .filter_map(|(key, _, content)| {
                self.index
                    .get(&(purpose.to_string(), key.to_string(), *content))
                    .copied()
            })
            .collect();
        let mut best = ids.iter().map(|&id| self.items[id].size).sum::<usize>() * MISS_SCALE;
        let mut chosen = None;
        for (node, cost) in self.cold_chains(&ids).into_iter().enumerate() {
            let Some(cost) = cost else { continue };
            if cost < best {
                best = cost;
                chosen = Some(NodeId(node));
            }
        }
        let mut keys = self.order(chosen);
        for &id in &ids {
            let key = self.items[id].key.clone();
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
        keys
    }

    #[cfg(test)]
    fn item_keys(&self) -> Vec<String> {
        self.items.iter().map(|item| item.key.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The digest a caller would pass: the text, and nothing about its name.
    fn d(text: &str) -> u64 {
        digest(text)
    }

    /// A set emitted for the first time comes out in the order it was given.
    #[test]
    fn the_first_plan_is_the_order_it_was_given() {
        let mut layout = Layout::new();
        let planned = layout.plan(
            "t",
            &[("a", 10, d("a")), ("b", 20, d("b")), ("c", 30, d("c"))],
        );
        assert_eq!(planned, vec!["a", "b", "c"]);
    }

    /// The same set twice is the same table twice: a stable prefix, not a new chain.
    #[test]
    fn a_repeated_set_is_emitted_unchanged() {
        let mut layout = Layout::new();
        let once = layout.plan(
            "t",
            &[("a", 10, d("a")), ("b", 20, d("b")), ("c", 30, d("c"))],
        );
        let chains = layout.chain_count();
        let again = layout.plan(
            "t",
            &[("a", 10, d("a")), ("b", 20, d("b")), ("c", 30, d("c"))],
        );
        assert_eq!(once, again);
        assert_eq!(layout.chain_count(), chains, "nothing new was learnt");
    }

    /// The order asked for loses to the order already cached: that is the point.
    #[test]
    fn a_warm_chain_beats_the_order_asked_for() {
        let mut layout = Layout::new();
        assert_eq!(
            layout.plan("t", &[("a", 10, d("a")), ("b", 20, d("b"))]),
            vec!["a", "b"]
        );
        assert_eq!(
            layout.plan("t", &[("b", 20, d("b")), ("a", 10, d("a"))]),
            vec!["a", "b"]
        );
    }

    /// A prefix that is partly warm keeps what it can and appends the rest.
    #[test]
    fn what_is_new_goes_after_what_is_warm() {
        let mut layout = Layout::new();
        layout.plan("t", &[("a", 10, d("a")), ("b", 20, d("b"))]);
        assert_eq!(
            layout.plan("t", &[("a", 10, d("a")), ("z", 5, d("z"))]),
            vec!["a", "z"]
        );
    }

    /// A row dropped from the set is never emitted, however warm its chain is.
    ///
    /// # Invariant
    ///
    /// The plan is a permutation of `wanted`: an uninstalled skill cannot come back
    /// because a past table mentioned it.
    #[test]
    fn a_chain_through_an_absent_item_is_not_a_candidate() {
        let mut layout = Layout::new();
        layout.plan(
            "t",
            &[("a", 10, d("a")), ("b", 20, d("b")), ("c", 30, d("c"))],
        );
        let planned = layout.plan("t", &[("c", 30, d("c"))]);
        assert_eq!(planned, vec!["c"]);
    }

    /// A changed size re-costs the chains through it instead of stranding them.
    #[test]
    fn a_shrunk_item_is_cheaper_than_sending_it_cold() {
        let mut layout = Layout::new();
        assert_eq!(layout.plan("t", &[("a", 1000, d("a"))]), vec!["a"]);
        let chains = layout.chain_count();
        assert_eq!(layout.plan("t", &[("a", 10, d("a"))]), vec!["a"]);
        assert_eq!(layout.chain_count(), chains, "re-costed, not abandoned");
    }

    /// What one run learnt is what the next run plans with.
    ///
    /// # Invariant
    ///
    /// A layout written to the store and read back plans the same order: a restart
    /// is a new process asking the same endpoint for the same bytes, so forgetting
    /// the chains would give every run a cold first request.
    #[test]
    fn a_layout_survives_a_round_trip_through_the_store() {
        let store =
            crate::store::Store::open(std::path::Path::new(":memory:")).expect("a throwaway store");
        let wanted = [("skill:a", 10usize, d("a")), ("skill:b", 20, d("b"))];
        let reversed = [("skill:b", 20usize, d("b")), ("skill:a", 10, d("a"))];
        let mut layout = Layout::new();
        assert_eq!(layout.plan("t", &wanted), vec!["skill:a", "skill:b"]);
        store
            .save_prefix_layout("probe", &layout)
            .expect("a writable store");
        let mut restored = store
            .prefix_layout("probe")
            .expect("a readable layout")
            .expect("the plan wrote one");
        restored.reindex();
        assert_eq!(restored.plan("t", &reversed), vec!["skill:a", "skill:b"]);
    }

    /// Edited content is a new item, not the old one under the same name.
    ///
    /// # Invariant
    ///
    /// A key that now names other bytes cannot be warmed by the chain that carried
    /// the old bytes: the cache would claim a prefix the endpoint cannot serve.
    #[test]
    fn changed_content_is_a_new_item() {
        let mut layout = Layout::new();
        layout.plan("t", &[("skill", 10, d("old body"))]);
        layout.plan("t", &[("skill", 10, d("new body"))]);
        assert_eq!(
            layout.chain_order("t", &[("skill", 10, d("new body"))]),
            vec!["skill"],
            "the new text is planned on its own"
        );
        let items = layout.item_keys();
        assert_eq!(items, vec!["skill".to_string()], "one name, one live item");
    }

    /// The item nothing wants any more is dropped, and its chains with it.
    ///
    /// # Invariant
    ///
    /// What stays is what was just asked for: an item whose text is gone cannot be
    /// revived by a later plan, so its chains are dead weight.
    #[test]
    fn what_is_no_longer_wanted_is_forgotten() {
        let mut layout = Layout::new();
        layout.plan("t", &[("a", 10, d("a")), ("b", 20, d("b"))]);
        assert_eq!(layout.item_keys(), vec!["a".to_string(), "b".to_string()]);
        layout.plan("t", &[("a", 10, d("a"))]);
        assert_eq!(layout.item_keys(), vec!["a".to_string()]);
    }

    /// Two callers in one session prune their own items and only their own.
    ///
    /// # Invariant
    ///
    /// The skill table and the prompt index both plan for the same session, so a
    /// plan by one must not forget the other's items: each would evict the other on
    /// every call, and neither would ever keep a chain.
    #[test]
    fn callers_do_not_prune_each_other() {
        let mut layout = Layout::new();
        layout.plan("skill table", &[("tools/build", 10, d("row"))]);
        layout.plan("prompt index", &[("@skill:x", 20, d("body"))]);
        assert_eq!(
            layout.item_keys(),
            vec!["tools/build".to_string(), "@skill:x".to_string()]
        );
        // And the second plan of one still beats a cold send of its own set.
        assert_eq!(
            layout.plan("prompt index", &[("@skill:x", 20, d("body"))]),
            vec!["@skill:x"]
        );
    }

    /// One session's chains are not another's offer.
    ///
    /// # Invariant
    ///
    /// A provider caches a conversation's prefix, so what this session emitted is
    /// the only thing warm for it; a chain from elsewhere must not be chosen.
    #[test]
    fn a_layout_belongs_to_one_session() {
        let store =
            crate::store::Store::open(std::path::Path::new(":memory:")).expect("a throwaway store");
        let wanted = [("a", 10usize, d("a")), ("b", 20, d("b"))];
        let reversed = [("b", 20usize, d("b")), ("a", 10, d("a"))];
        assert_eq!(plan(&store, "one", "t", &wanted), vec!["a", "b"]);
        assert_eq!(
            plan(&store, "two", "t", &reversed),
            vec!["b", "a"],
            "the other session kept the order it asked for"
        );
        assert_eq!(plan(&store, "one", "t", &reversed), vec!["a", "b"]);
    }

    #[test]
    fn an_empty_set_plans_nothing() {
        let mut layout = Layout::new();
        assert!(layout.plan("t", &[]).is_empty());
    }
}
