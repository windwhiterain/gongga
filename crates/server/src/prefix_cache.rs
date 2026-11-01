//! Which order to emit a set in, so the prefix cache still holds most of it.
//!
//! See `docs/notes/server-prefix-cache.md`.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, MutexGuard, OnceLock};

use serde::{Deserialize, Serialize};

use crate::store::Store;

/// One layout for the whole home: on disk, because a restart asks again.
fn shared(store: &Store) -> MutexGuard<'static, Layout> {
    static SHARED: OnceLock<Mutex<Layout>> = OnceLock::new();
    SHARED
        .get_or_init(|| {
            Mutex::new(match store.prefix_layout() {
                Ok(Some(layout)) => layout,
                Ok(None) => Layout::new(),
                Err(e) => {
                    tracing::warn!(error = %e, "the stored prefix layout cannot be read; starting empty");
                    Layout::new()
                }
            })
        })
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// The cheapest order for `wanted`, learnt across callers and runs.
pub fn plan(store: &Store, wanted: &[(&str, usize)]) -> Vec<String> {
    let mut layout = shared(store);
    let order = layout.plan(wanted);
    if let Err(e) = store.save_prefix_layout(&layout) {
        tracing::warn!(error = %e, "the learnt prefix layout could not be written");
    }
    order
}

/// What a byte sent again costs against a byte the cache already holds.
///
/// liana's `cache_miss_cost_scale`.
const MISS_SCALE: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct NodeId(usize);

/// One thing that can be emitted: a table row, a memory, a message.
#[derive(Debug, Serialize, Deserialize)]
struct Item {
    key: String,
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
    index: HashMap<String, usize>,
    nodes: Vec<Node>,
}

impl Layout {
    pub fn new() -> Self {
        Self::default()
    }

    /// The cheapest chain, then whatever is new. See the note.
    pub fn plan(&mut self, wanted: &[(&str, usize)]) -> Vec<String> {
        let ids = self.ids(wanted);
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
        self.order(tail)
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
    fn ids(&mut self, wanted: &[(&str, usize)]) -> Vec<usize> {
        let mut ids: Vec<usize> = Vec::with_capacity(wanted.len());
        let mut restated = false;
        for &(key, size) in wanted {
            let id = match self.index.get(key) {
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
                        key: key.to_string(),
                        nodes: Vec::new(),
                        size,
                    });
                    self.index.insert(key.to_string(), id);
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
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A set emitted for the first time comes out in the order it was given.
    #[test]
    fn the_first_plan_is_the_order_it_was_given() {
        let mut layout = Layout::new();
        let planned = layout.plan(&[("a", 10), ("b", 20), ("c", 30)]);
        assert_eq!(planned, vec!["a", "b", "c"]);
    }

    /// The same set twice is the same table twice: a stable prefix, not a new chain.
    #[test]
    fn a_repeated_set_is_emitted_unchanged() {
        let mut layout = Layout::new();
        let once = layout.plan(&[("a", 10), ("b", 20), ("c", 30)]);
        let chains = layout.chain_count();
        let again = layout.plan(&[("a", 10), ("b", 20), ("c", 30)]);
        assert_eq!(once, again);
        assert_eq!(layout.chain_count(), chains, "nothing new was learnt");
    }

    /// The order asked for loses to the order already cached: that is the point.
    #[test]
    fn a_warm_chain_beats_the_order_asked_for() {
        let mut layout = Layout::new();
        assert_eq!(layout.plan(&[("a", 10), ("b", 20)]), vec!["a", "b"]);
        assert_eq!(layout.plan(&[("b", 20), ("a", 10)]), vec!["a", "b"]);
    }

    /// A prefix that is partly warm keeps what it can and appends the rest.
    #[test]
    fn what_is_new_goes_after_what_is_warm() {
        let mut layout = Layout::new();
        layout.plan(&[("a", 10), ("b", 20)]);
        assert_eq!(layout.plan(&[("a", 10), ("z", 5)]), vec!["a", "z"]);
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
        layout.plan(&[("a", 10), ("b", 20), ("c", 30)]);
        let planned = layout.plan(&[("c", 30)]);
        assert_eq!(planned, vec!["c"]);
    }

    /// A changed size re-costs the chains through it instead of stranding them.
    #[test]
    fn a_shrunk_item_is_cheaper_than_sending_it_cold() {
        let mut layout = Layout::new();
        assert_eq!(layout.plan(&[("a", 1000)]), vec!["a"]);
        let chains = layout.chain_count();
        assert_eq!(layout.plan(&[("a", 10)]), vec!["a"]);
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
        let wanted = [("skill:a", 10usize), ("skill:b", 20)];
        let reversed = [("skill:b", 20usize), ("skill:a", 10)];
        assert_eq!(plan(&store, &wanted), vec!["skill:a", "skill:b"]);
        let stored = store.prefix_layout().expect("a readable layout");
        let mut restored = stored.expect("the plan wrote one");
        assert_eq!(restored.plan(&reversed), vec!["skill:a", "skill:b"]);
    }

    #[test]
    fn an_empty_set_plans_nothing() {
        let mut layout = Layout::new();
        assert!(layout.plan(&[]).is_empty());
    }
}
