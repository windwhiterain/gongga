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
    rates: Rates,
) -> Vec<String> {
    let mut layouts = shared(store, session);
    let Some(layout) = layouts.get_mut(session) else {
        return Vec::new();
    };
    let order = layout.plan(purpose, wanted, rates, crate::store::now_millis() as u64);
    if let Err(e) = store.save_prefix_layout(session, layout) {
        tracing::warn!(error = %e, session = %session, "the learnt prefix layout could not be written");
    }
    order
}

/// What a plan is priced at: the roster's rates, and how long a cache lasts.
#[derive(Debug, Clone, Copy, Default)]
pub struct Rates {
    /// What the roster says this model charges, when it says anything.
    pub price: Option<crate::providers::Price>,
    /// The chance a prefix the provider once held is gone by the next request.
    pub invalid: f64,
    /// How long a prefix survives, in millis. `0` never expires.
    pub ttl_ms: u64,
}

/// What a cached token is worth when the roster states no rate: a stand-in.
pub const DEFAULT_MISS_RATIO: f64 = 10.0;

impl Rates {
    /// How many fresh tokens one cached token saves, with the invalidation folded in.
    ///
    /// # Invariant
    ///
    /// A cache expected to be gone `invalid` of the time is worth `(1 - invalid)` of
    /// its rate, so a hit is worth `ratio / ((1 - invalid) + invalid * ratio)`.
    pub fn scale(&self) -> f64 {
        let ratio = self
            .price
            .map(|p| p.miss_ratio())
            .filter(|ratio| *ratio > 1.0)
            .unwrap_or(DEFAULT_MISS_RATIO);
        let invalid = self.invalid.clamp(0.0, 1.0);
        let expected = (1.0 - invalid) + invalid * ratio;
        (ratio / expected).max(1.0)
    }
}

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
    /// What sending it costs: tokens the provider counted, not bytes estimated here.
    tokens: usize,
}

#[derive(Debug, Serialize, Deserialize)]
struct Node {
    item: usize,
    parent: Option<NodeId>,
    children: Vec<NodeId>,
    /// Every ancestor's size and its own: what a chain ending here carries.
    size: usize,
    /// When this chain was last sent, in millis since the epoch.
    ///
    /// # Invariant
    ///
    /// Using a chain refreshes its ancestors, so `touched` never decreases towards
    /// the root: a node is stale exactly when its own prefix expired.
    touched: u64,
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
    pub fn plan(
        &mut self,
        purpose: &str,
        wanted: &[(&str, usize, u64)],
        rates: Rates,
        now: u64,
    ) -> Vec<String> {
        // First, because everything after reads node indices.
        self.expire(now, rates.ttl_ms);
        let scale = rates.scale();
        let ids = self.ids(purpose, wanted);
        if ids.is_empty() {
            return Vec::new();
        }
        // What sending all of it with nothing cached costs: the floor to beat.
        let mut best: f64 =
            ids.iter().map(|&id| self.items[id].tokens).sum::<usize>() as f64 * scale;
        let mut chosen: Option<NodeId> = None;
        for (node, cost) in self.cold_chains(&ids, scale).into_iter().enumerate() {
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
            // Sent again, so the provider holds it again: the chain is fresh now.
            self.nodes[node.0].touched = now;
            walk = self.nodes[node.0].parent;
        }
        let mut tail = chosen;
        for &id in &ids {
            if !warm.contains(&id) {
                tail = Some(self.add(id, tail, now));
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
    fn cold_chains(&self, ids: &[usize], scale: f64) -> Vec<Option<f64>> {
        let mut allowed = vec![true; self.nodes.len()];
        for id in 0..self.items.len() {
            if ids.contains(&id) {
                continue;
            }
            for node in self.subtree(id) {
                allowed[node] = false;
            }
        }
        let mut costs: Vec<Option<f64>> = self
            .nodes
            .iter()
            .enumerate()
            .map(|(node, n)| allowed[node].then_some(n.size as f64))
            .collect();
        for &id in ids {
            let carried = self.subtree_marks(id);
            let tokens = self.items[id].tokens;
            for (node, cost) in costs.iter_mut().enumerate() {
                if let Some(cost) = cost {
                    if !carried[node] {
                        *cost += tokens as f64 * scale;
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
        for &(key, tokens, content) in wanted {
            let id = match self
                .index
                .get(&(purpose.to_string(), key.to_string(), content))
            {
                Some(&id) => {
                    if self.items[id].tokens != tokens {
                        self.items[id].tokens = tokens;
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
                        tokens,
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
        self.sweep(doomed);
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
            .map(|(_, item)| item)
            .collect();
        for node in &mut self.nodes {
            node.item = reindex[node.item].expect("a kept node's item was kept");
        }
        self.reindex();
    }

    /// Removes the nodes `doomed` marks and renumbers what is left.
    ///
    /// # Invariant
    ///
    /// One writer for node renumbering, so an item's own list and a node's own indices
    /// cannot disagree; an item left with no nodes simply has no chain to be chosen.
    fn sweep(&mut self, doomed: Vec<bool>) {
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
                touched: node.touched,
            })
            .collect();
        for item in &mut self.items {
            item.nodes = item
                .nodes
                .iter()
                .filter_map(|node| remap[node.0].map(NodeId))
                .collect();
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
            self.nodes[node].size = above + self.items[item].tokens;
        }
    }

    fn add(&mut self, item: usize, parent: Option<NodeId>, now: u64) -> NodeId {
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
            size: carried + self.items[item].tokens,
            touched: now,
        });
        self.items[item].nodes.push(id);
        id
    }

    /// Drops every chain whose prefix is older than `ttl_ms`.
    ///
    /// # Invariant
    ///
    /// A stale node takes its whole subtree with it: a provider's cache is a prefix,
    /// so if the front of a chain expired the rest of it cannot be served from cache
    /// either. `0` never expires -- a lifetime nobody stated is not one to invent.
    fn expire(&mut self, now: u64, ttl_ms: u64) {
        if ttl_ms == 0 {
            return;
        }
        let cutoff = now.saturating_sub(ttl_ms);
        let mut doomed = vec![false; self.nodes.len()];
        for node in 0..self.nodes.len() {
            // Parents are always older in the array, so their verdict is already in.
            let above = self.nodes[node]
                .parent
                .is_some_and(|parent| doomed[parent.0]);
            doomed[node] = above || self.nodes[node].touched < cutoff;
        }
        if doomed.contains(&true) {
            self.sweep(doomed);
        }
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
    fn chain_order(
        &self,
        purpose: &str,
        wanted: &[(&str, usize, u64)],
        rates: Rates,
        now: u64,
    ) -> Vec<String> {
        let ids: Vec<usize> = wanted
            .iter()
            .filter_map(|(key, _, content)| {
                self.index
                    .get(&(purpose.to_string(), key.to_string(), *content))
                    .copied()
            })
            .collect();
        // A chain that expired is not one this plan could choose.
        let cutoff = if rates.ttl_ms == 0 {
            0
        } else {
            now.saturating_sub(rates.ttl_ms)
        };
        let stale: Vec<bool> = (0..self.nodes.len())
            .map(|node| {
                self.nodes[node].touched < cutoff
                    || self.nodes[node]
                        .parent
                        .is_some_and(|parent| self.nodes[parent.0].touched < cutoff)
            })
            .collect();
        let scale = rates.scale();
        let mut best = ids.iter().map(|&id| self.items[id].tokens).sum::<usize>() as f64 * scale;
        let mut chosen = None;
        for (node, cost) in self.cold_chains(&ids, scale).into_iter().enumerate() {
            let Some(cost) = cost else { continue };
            if stale[node] {
                continue;
            }
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

    /// The clock a plan runs at, so a test's staleness is its own to state.
    const NOW: u64 = 1_000_000;

    /// A 10x provider, with `invalid` standing in for how sure the cache is.
    fn sure(invalid: f64) -> Rates {
        Rates {
            price: Some(crate::providers::Price {
                input: Some(1.0),
                cache_read: Some(0.1),
                output: None,
            }),
            invalid,
            ttl_ms: 0,
        }
    }

    /// The scale a plan runs at when the roster states no price: the stand-in.
    const SCALE: f64 = DEFAULT_MISS_RATIO;

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
            Rates::default(),
            NOW,
        );
        assert_eq!(planned, vec!["a", "b", "c"]);
    }

    /// The same set twice is the same table twice: a stable prefix, not a new chain.
    #[test]
    fn a_repeated_set_is_emitted_unchanged() {
        let mut layout = Layout::new();
        let set = [("a", 10, d("a")), ("b", 20, d("b")), ("c", 30, d("c"))];
        let once = layout.plan("t", &set, Rates::default(), NOW);
        let chains = layout.chain_count();
        let again = layout.plan("t", &set, Rates::default(), NOW);
        assert_eq!(once, again);
        assert_eq!(layout.chain_count(), chains, "nothing new was learnt");
    }

    /// The order asked for loses to the order already cached: that is the point.
    #[test]
    fn a_warm_chain_beats_the_order_asked_for() {
        let mut layout = Layout::new();
        assert_eq!(
            layout.plan(
                "t",
                &[("a", 10, d("a")), ("b", 20, d("b"))],
                Rates::default(),
                NOW
            ),
            vec!["a", "b"]
        );
        assert_eq!(
            layout.plan(
                "t",
                &[("b", 20, d("b")), ("a", 10, d("a"))],
                Rates::default(),
                NOW
            ),
            vec!["a", "b"]
        );
    }

    /// A prefix that is partly warm keeps what it can and appends the rest.
    #[test]
    fn what_is_new_goes_after_what_is_warm() {
        let mut layout = Layout::new();
        layout.plan(
            "t",
            &[("a", 10, d("a")), ("b", 20, d("b"))],
            Rates::default(),
            NOW,
        );
        assert_eq!(
            layout.plan(
                "t",
                &[("a", 10, d("a")), ("z", 5, d("z"))],
                Rates::default(),
                NOW
            ),
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
            Rates::default(),
            NOW,
        );
        assert_eq!(
            layout.plan("t", &[("c", 30, d("c"))], Rates::default(), NOW),
            vec!["c"]
        );
    }

    /// A changed size re-costs the chains through it instead of stranding them.
    #[test]
    fn a_shrunk_item_is_cheaper_than_sending_it_cold() {
        let mut layout = Layout::new();
        assert_eq!(
            layout.plan("t", &[("a", 1000, d("a"))], Rates::default(), NOW),
            vec!["a"]
        );
        let chains = layout.chain_count();
        assert_eq!(
            layout.plan("t", &[("a", 10, d("a"))], Rates::default(), NOW),
            vec!["a"]
        );
        assert_eq!(layout.chain_count(), chains, "re-costed, not abandoned");
    }

    /// The roster's ratio is what decides, not a constant of this module's.
    ///
    /// # Invariant
    ///
    /// A scale of one says a cache is worth nothing, and then the order asked for
    /// wins; a scale above one says a warm prefix is worth a detour through it.
    #[test]
    fn the_scale_decides_what_a_warm_prefix_is_worth() {
        let mut cheap = Layout::new();
        assert_eq!(
            cheap.plan("t", &[("a", 10, d("a")), ("b", 20, d("b"))], sure(1.0), NOW),
            vec!["a", "b"]
        );
        assert_eq!(
            cheap.plan("t", &[("b", 20, d("b")), ("a", 10, d("a"))], sure(1.0), NOW),
            vec!["b", "a"],
            "nothing warm is worth anything, so the caller's order stands"
        );
        let mut dear = Layout::new();
        dear.plan("t", &[("a", 10, d("a")), ("b", 20, d("b"))], sure(0.0), NOW);
        assert_eq!(
            dear.plan("t", &[("b", 20, d("b")), ("a", 10, d("a"))], sure(0.0), NOW),
            vec!["a", "b"],
            "a hit is worth ten misses, so the cached order is worth following"
        );
    }

    /// The scale comes from the roster's two rates, and the invalidation lowers it.
    #[test]
    fn rates_turn_prices_and_invalidation_into_a_scale() {
        // A ratio that is not the stand-in, so this cannot pass by coincidence.
        let price = crate::providers::Price {
            input: Some(3.0),
            cache_read: Some(0.5),
            output: None,
        };
        let dear = Rates {
            price: Some(price),
            invalid: 0.0,
            ttl_ms: 0,
        };
        assert!((dear.scale() - 6.0).abs() < 1e-9, "{}", dear.scale());
        let half = Rates {
            price: Some(price),
            invalid: 0.5,
            ttl_ms: 0,
        };
        // Half the time the cache is gone, so a hit is worth half of six.
        assert!((half.scale() - 6.0 / 3.5).abs() < 1e-9, "{}", half.scale());
        let gone = Rates {
            price: Some(price),
            invalid: 1.0,
            ttl_ms: 0,
        };
        assert_eq!(
            gone.scale(),
            1.0,
            "a cache that never holds is worth nothing"
        );
        assert_eq!(
            Rates::default().scale(),
            DEFAULT_MISS_RATIO,
            "no price falls back to the stand-in"
        );
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
        layout.plan("t", &[("skill", 10, d("old body"))], Rates::default(), NOW);
        layout.plan("t", &[("skill", 10, d("new body"))], Rates::default(), NOW);
        assert_eq!(
            layout.chain_order("t", &[("skill", 10, d("new body"))], Rates::default(), NOW),
            vec!["skill"],
            "the new text is planned on its own"
        );
        assert_eq!(
            layout.item_keys(),
            vec!["skill".to_string()],
            "one name, one live item"
        );
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
        layout.plan(
            "t",
            &[("a", 10, d("a")), ("b", 20, d("b"))],
            Rates::default(),
            NOW,
        );
        assert_eq!(layout.item_keys(), vec!["a".to_string(), "b".to_string()]);
        layout.plan("t", &[("a", 10, d("a"))], Rates::default(), NOW);
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
        layout.plan(
            "skill table",
            &[("tools/build", 10, d("row"))],
            Rates::default(),
            NOW,
        );
        layout.plan(
            "prompt index",
            &[("@skill:x", 20, d("body"))],
            Rates::default(),
            NOW,
        );
        assert_eq!(
            layout.item_keys(),
            vec!["tools/build".to_string(), "@skill:x".to_string()]
        );
        assert_eq!(
            layout.plan(
                "prompt index",
                &[("@skill:x", 20, d("body"))],
                Rates::default(),
                NOW
            ),
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
        assert_eq!(
            plan(&store, "sessions/a", "t", &wanted, Rates::default()),
            vec!["a", "b"]
        );
        assert_eq!(
            plan(&store, "sessions/b", "t", &reversed, Rates::default()),
            vec!["b", "a"],
            "the other session kept the order it asked for"
        );
        assert_eq!(
            plan(&store, "sessions/a", "t", &reversed, Rates::default()),
            vec!["a", "b"]
        );
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
        assert_eq!(
            layout.plan("t", &wanted, Rates::default(), NOW),
            vec!["skill:a", "skill:b"]
        );
        store
            .save_prefix_layout("round-trip", &layout)
            .expect("a writable store");
        let mut restored = store
            .prefix_layout("round-trip")
            .expect("a readable layout")
            .expect("the plan wrote one");
        restored.reindex();
        assert_eq!(
            restored.plan("t", &reversed, Rates::default(), NOW),
            vec!["skill:a", "skill:b"]
        );
    }

    /// A measurement from the provider becomes the ratio a plan is measured in.
    #[test]
    fn a_measured_ratio_replaces_the_stand_in() {
        let store =
            crate::store::Store::open(std::path::Path::new(":memory:")).expect("a throwaway store");
        assert!(
            (store.token_ratio("ratio-model") - 1.0 / crate::store::CHARS_PER_TOKEN).abs() < 1e-9,
            "unmeasured falls back"
        );
        store
            .note_token_ratio("ratio-model", 4_000, 1_000)
            .expect("a write");
        assert!((store.token_ratio("ratio-model") - 0.25).abs() < 1e-9);
        store
            .note_token_ratio("ratio-model", 4_000, 1_000)
            .expect("a second write");
        assert!(
            (store.token_ratio("ratio-model") - 0.25).abs() < 1e-9,
            "ratios, not sums"
        );
    }

    #[test]
    fn an_empty_set_plans_nothing() {
        let mut layout = Layout::new();
        assert!(layout.plan("t", &[], Rates::default(), NOW).is_empty());
    }
}
