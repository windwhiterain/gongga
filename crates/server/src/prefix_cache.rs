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
            Ok(Some(layout)) => layout,
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

/// The digest that says "the same text as last time", for an item's text.
fn digest(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

/// One thing a caller wants emitted, and what it says now.
pub struct Wanted<'a> {
    /// The name the caller knows it by, which may come to name other text later.
    pub key: &'a str,
    /// The text as it stands now.
    pub text: &'a str,
    /// What sending that text costs: tokens the provider counted, not bytes.
    pub tokens: usize,
}

/// Something the request carries behind what this module orders: one transcript row.
///
/// # Invariant
///
/// Carried in the order given, never reordered and never emitted by this module. It is
/// here so its node exists: the provider holds that prefix, and only a node can say so.
pub struct Carried<'a> {
    /// The row this is, which is what the provider's prefix through it is keyed by.
    pub key: &'a str,
    pub tokens: usize,
}

/// What to emit: each entry's name and the text to send under it, and the patches
/// that catch up what changed.
///
/// # Invariant
///
/// The caller sends `entries` and appends `revisions` after them: a patch names the
/// entry it belongs to, so the two lists stay one-to-one by name, never by position.
#[derive(Debug, Default)]
pub struct Plan {
    pub entries: Vec<(String, String)>,
    pub revisions: Vec<(String, String)>,
}

/// The cheapest order for `wanted`, learnt across what this session has emitted.
///
/// # Invariant
///
/// `tolerance` is the largest share of an entry's lines that may change and still be
/// appended as a patch rather than re-sent: `None` makes edited text a new item.
pub fn plan(
    store: &Store,
    session: &str,
    purpose: &str,
    wanted: &[Wanted<'_>],
    carried: &[Carried<'_>],
    rates: Rates,
    tolerance: Option<f64>,
) -> Plan {
    let mut layouts = shared(store, session);
    let Some(layout) = layouts.get_mut(session) else {
        return Plan::default();
    };
    let planned = layout.plan(
        purpose,
        wanted,
        carried,
        rates,
        tolerance,
        crate::store::now_millis() as u64,
    );
    if let Err(e) = store.save_prefix_layout(session, layout) {
        tracing::warn!(error = %e, session = %session, "the learnt prefix layout could not be written");
    }
    planned
}

/// What a plan is priced at: the roster's rates, and how long a cache lasts.
#[derive(Debug, Clone, Copy, Default)]
pub struct Rates {
    /// What the roster says this model charges, when it says anything.
    pub price: Option<crate::providers::Price>,
    /// The chance a prefix the provider once held is gone by the next request.
    pub invalid: f64,
    /// How long a prefix survives in the tier this server buys; `0` never expires. See
    /// `docs/notes/server-prefix-cache.md`.
    pub ttl_ms: u64,
    /// Tokens this request carries after the index: the transcript and the card.
    ///
    /// # Invariant
    ///
    /// Priced only as what changing the index costs, because the index sits ahead of
    /// the transcript in the provider's prefix: an emission that differs from the one
    /// it holds is a miss for every byte behind it, not only for the index.
    pub following_tokens: usize,
}

/// The purpose under which the rows a request carries are kept.
const CARRIED: &str = "request tail";

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
    /// The text this item puts in its place: an edit within tolerance keeps the text
    /// the provider holds.
    pub text: String,
    /// Carried after what this module orders, so it is never an ordering choice.
    #[serde(default)]
    pinned: bool,
    /// One node per order this item has ended.
    nodes: Vec<NodeId>,
    /// What sending it costs: tokens the provider counted, not bytes estimated here.
    tokens: usize,
}

/// One entry whose text in the index is out of date, and the change since.
struct Revision {
    item: usize,
    patch: String,
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
    /// The item a name resolves to now, which is what an edited entry is measured
    /// against.
    #[serde(skip)]
    named: HashMap<(String, String), usize>,
    nodes: Vec<Node>,
    /// The item ids the last emission carried, in order; empty when no emission has
    /// been made under this layout yet.
    #[serde(default)]
    last: Vec<usize>,
}

impl Layout {
    pub fn new() -> Self {
        Self::default()
    }

    /// Rebuilds both lookups from `items`, for a layout that came off disk.
    pub fn reindex(&mut self) {
        self.index.clear();
        self.named.clear();
        for (id, item) in self.items.iter().enumerate() {
            let name = (item.purpose.clone(), item.key.clone());
            self.index.insert(
                (item.purpose.clone(), item.key.clone(), digest(&item.text)),
                id,
            );
            self.named.insert(name, id);
        }
    }

    /// The cheapest chain, then whatever is new. See the note.
    pub fn plan(
        &mut self,
        purpose: &str,
        wanted: &[Wanted<'_>],
        carried: &[Carried<'_>],
        rates: Rates,
        tolerance: Option<f64>,
        now: u64,
    ) -> Plan {
        // First, because everything after reads node indices.
        self.expire(now, rates.ttl_ms);
        let scale = rates.scale();
        let (ids, revisions) = self.ids(purpose, wanted, tolerance);
        if ids.is_empty() {
            return Plan::default();
        }
        let following = rates.following_tokens as f64;
        // What sending all of it with nothing cached costs: the floor to beat.
        let baseline = self.sequence(None, &ids);
        let mut best: f64 = ids.iter().map(|&id| self.items[id].tokens).sum::<usize>() as f64
            * scale
            + self.paid_behind(&baseline, following, scale);
        let mut chosen: Option<NodeId> = None;
        for (node, cost) in self.cold_chains(&ids, scale).into_iter().enumerate() {
            let Some(cost) = cost else { continue };
            let sequence = self.sequence(Some(NodeId(node)), &ids);
            let cost = cost + self.paid_behind(&sequence, following, scale);
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
        // Behind every ordering: the rows this request carries, which sit after it.
        tail = self.carry(tail, carried, now);
        let entries = self.order(tail);
        self.last = self.sequence(tail, &ids);
        // After the plan, never before: forgetting first would renumber the items
        // these ids name.
        self.forget_unwanted(purpose, &ids);
        let carried_ids = self.carried_ids(carried);
        self.forget_unwanted(CARRIED, &carried_ids);
        Plan {
            revisions: self.place(&entries, revisions),
            entries,
        }
    }

    /// The revisions in the order their entries are emitted.
    ///
    /// # Invariant
    ///
    /// A patch is appended in the order of the entries it belongs to, so one entry's
    /// diff can never land under another entry's name.
    fn place(
        &self,
        entries: &[(String, String)],
        revisions: Vec<Revision>,
    ) -> Vec<(String, String)> {
        let at: HashMap<&str, usize> = entries
            .iter()
            .enumerate()
            .map(|(at, (key, _))| (key.as_str(), at))
            .collect();
        let mut placed: Vec<(usize, String, String)> = revisions
            .into_iter()
            .filter_map(|revision| {
                let key = self.items[revision.item].key.clone();
                at.get(key.as_str()).map(|at| (*at, key, revision.patch))
            })
            .collect();
        placed.sort_by_key(|(at, _, _)| *at);
        placed
            .into_iter()
            .map(|(_, key, patch)| (key, patch))
            .collect()
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
            if ids.contains(&id) || self.items[id].pinned {
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

    /// The items `carried` names, creating what is new.
    fn carried_ids(&self, carried: &[Carried<'_>]) -> Vec<usize> {
        carried
            .iter()
            .filter_map(|row| {
                self.named
                    .get(&(CARRIED.to_string(), row.key.to_string()))
                    .copied()
            })
            .collect()
    }

    /// Extends the emission with the rows this request carries, touching each.
    ///
    /// # Invariant
    ///
    /// A row already hanging under this chain is reused rather than re-added, and every
    /// node on the way to it is touched: the provider holds each of those prefixes again.
    fn carry(&mut self, tail: Option<NodeId>, carried: &[Carried<'_>], now: u64) -> Option<NodeId> {
        let mut tail = tail;
        for row in carried {
            let name = (CARRIED.to_string(), row.key.to_string());
            let id = match self.named.get(&name) {
                Some(&id) => {
                    if self.items[id].tokens != row.tokens {
                        self.items[id].tokens = row.tokens;
                        self.recost();
                    }
                    id
                }
                None => {
                    let id = self.items.len();
                    self.items.push(Item {
                        purpose: CARRIED.to_string(),
                        key: row.key.to_string(),
                        text: String::new(),
                        pinned: true,
                        nodes: Vec::new(),
                        tokens: row.tokens,
                    });
                    self.named.insert(name, id);
                    id
                }
            };
            let reused = tail.and_then(|parent| {
                self.nodes[parent.0]
                    .children
                    .iter()
                    .find(|child| self.nodes[child.0].item == id)
                    .copied()
            });
            tail = Some(match reused {
                Some(node) => {
                    let mut walk = Some(node);
                    while let Some(node) = walk {
                        self.nodes[node.0].touched = now;
                        walk = self.nodes[node.0].parent;
                    }
                    node
                }
                None => self.add(id, tail, now),
            });
        }
        tail
    }

    /// The index items an emission under `chain` would carry, in order.
    ///
    /// # Invariant
    ///
    /// Index only: a carried row grows every step, and a question about what the index
    /// costs must not change its answer because the transcript did.
    fn sequence(&self, chain: Option<NodeId>, ids: &[usize]) -> Vec<usize> {
        let mut carried: Vec<usize> = Vec::new();
        let mut walk = chain;
        while let Some(node) = walk {
            let item = self.nodes[node.0].item;
            if !self.items[item].pinned {
                carried.push(item);
            }
            walk = self.nodes[node.0].parent;
        }
        carried.reverse();
        for &id in ids {
            if !carried.contains(&id) {
                carried.push(id);
            }
        }
        carried
    }

    /// What the payload behind the index costs under this emission.
    ///
    /// # Invariant
    ///
    /// Warm at the hit rate only for the emission the provider already holds; every
    /// other one pays the miss rate for the whole transcript.
    fn paid_behind(&self, sequence: &[usize], following: f64, scale: f64) -> f64 {
        if sequence == self.last {
            following
        } else {
            following * scale
        }
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
    ///
    /// # Invariant
    ///
    /// An entry whose text changed keeps the item the provider already holds when the
    /// change fits `tolerance`: its chain stays a candidate and the patch is appended
    /// last, where a change costs the tail rather than the prefix.
    fn ids(
        &mut self,
        purpose: &str,
        wanted: &[Wanted<'_>],
        tolerance: Option<f64>,
    ) -> (Vec<usize>, Vec<Revision>) {
        let mut ids: Vec<usize> = Vec::with_capacity(wanted.len());
        let mut revisions: Vec<Revision> = Vec::new();
        let mut restated = false;
        for want in wanted {
            let content = digest(want.text);
            let name = (purpose.to_string(), want.key.to_string());
            let id = match self
                .index
                .get(&(purpose.to_string(), want.key.to_string(), content))
            {
                Some(&id) => {
                    if self.items[id].tokens != want.tokens {
                        self.items[id].tokens = want.tokens;
                        restated = true;
                    }
                    id
                }
                None => match self.warm(purpose, want, tolerance) {
                    Some((id, patch)) => {
                        if !patch.is_empty() {
                            revisions.push(Revision { item: id, patch });
                        }
                        id
                    }
                    None => {
                        let id = self.items.len();
                        self.items.push(Item {
                            purpose: purpose.to_string(),
                            key: want.key.to_string(),
                            text: want.text.to_string(),
                            pinned: false,
                            nodes: Vec::new(),
                            tokens: want.tokens,
                        });
                        self.index
                            .insert((purpose.to_string(), want.key.to_string(), content), id);
                        self.named.insert(name, id);
                        id
                    }
                },
            };
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        if restated {
            self.recost();
        }
        (ids, revisions)
    }

    /// The item a name already resolves to, and the patch that catches it up.
    ///
    /// # Invariant
    ///
    /// A tolerated entry is measured against the text the provider holds, which is
    /// the item's own: never against a version this server did not send.
    fn warm(
        &self,
        purpose: &str,
        want: &Wanted<'_>,
        tolerance: Option<f64>,
    ) -> Option<(usize, String)> {
        let tolerance = tolerance?;
        let id = *self
            .named
            .get(&(purpose.to_string(), want.key.to_string()))?;
        let patch = crate::diff::patch(&self.items[id].text, want.text, tolerance)?;
        Some((id, patch))
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
        // An emission with a dropped item cannot be repeated, so it no longer counts.
        self.last = self.last.iter().filter_map(|item| reindex[*item]).collect();
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
        if !doomed.contains(&true) {
            return;
        }
        let held = self.nodes.len();
        self.sweep(doomed);
        tracing::debug!(
            ttl_ms,
            dropped = held - self.nodes.len(),
            held,
            "prefix cache expired"
        );
    }

    fn order(&self, tail: Option<NodeId>) -> Vec<(String, String)> {
        let mut entries = Vec::new();
        let mut walk = tail;
        while let Some(node) = walk {
            let item = &self.items[self.nodes[node.0].item];
            // What this module orders is the index: a carried row is somebody else's.
            if !item.pinned {
                entries.push((item.key.clone(), item.text.clone()));
            }
            walk = self.nodes[node.0].parent;
        }
        entries.reverse();
        entries
    }
}
