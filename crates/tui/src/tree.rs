//! The session list as a tree: every prefix of a name is a row.
//!
//! # Invariant
//!
//! One projection per layout: [`rows`] is the only place a name is cut into
//! segments, and the frame, the click and the scroll bound all read what it
//! returned, so a row cannot be drawn on one line and clicked on another.
//! See `docs/notes/tui-tree.md`.

use std::collections::{BTreeSet, HashMap};

use crate::Listed;

/// What a session is doing, as the list shows it in one column.
///
/// # Invariant
///
/// Ordered by how much attention it asks for, so a group row reports the strongest
/// of what it holds without a second order to keep in step with this one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Mark {
    /// A session that needs nothing, and a group whose sessions all do.
    #[default]
    Idle,
    /// At least one session under this row is working.
    Running,
    /// A session under this row stopped while it was not shown.
    Stopped,
    /// At least one session under this row stopped with an error.
    Error,
}

impl Mark {
    /// The status word the announcement used, as a mark. An unknown word is idle.
    pub fn of(word: &str) -> Self {
        match word {
            "running" => Mark::Running,
            "error" => Mark::Error,
            _ => Mark::Idle,
        }
    }
}

/// One row of the list: a session, a group, or a name that is both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// The name this row stands for: a session's own name, or a prefix of one.
    pub name: String,
    /// The last segment of `name`, which is what the row draws.
    pub label: String,
    /// How many segments precede `label`; the row's indent is two columns each.
    pub depth: usize,
    /// Whether a session is named exactly this -- and not only its children.
    pub session: bool,
    /// Whether any name goes on below this row.
    pub group: bool,
    /// The strongest mark of this row and everything under it.
    pub mark: Mark,
    /// Whether this row's children are folded away. Only a group can be.
    pub collapsed: bool,
}

impl Row {
    /// The columns this row's segment is indented by: two for each group above it.
    pub fn indent(&self) -> usize {
        2 * self.depth
    }

    /// The columns before this row's marker, counted from the list's left edge.
    ///
    /// # Invariant
    ///
    /// The frame and the click both place the marker here: the status column and its
    /// space come first, then the indent.
    pub fn marker_column(&self) -> usize {
        2 + self.indent()
    }

    /// The column a fold is drawn in: `|` when this row is folded, nothing otherwise.
    ///
    /// # Invariant
    ///
    /// Nothing at all on an expanded row, because a column held open for a gesture is
    /// a column the names do not get. The label starts in this column instead, so a
    /// folded row's label sits one column right of its siblings' -- which is what
    /// `|lab` above `lab` means.
    pub fn marker(&self) -> &'static str {
        match self.group && self.collapsed {
            true => "|",
            false => "",
        }
    }
}

/// One node of the tree being built, before it is a row.
struct Node {
    name: String,
    label: String,
    depth: usize,
    children: Vec<usize>,
    session: bool,
    /// What this node's own session is doing; `Idle` when it names no session.
    own: Mark,
    /// The strongest mark of everything under this node, itself included.
    under: Mark,
}

/// The rows the list draws: every session's name, and every prefix of one.
///
/// # Invariant
///
/// Siblings stand in the layout's order, oldest first, so a group appears where its
/// first session is; a folded name keeps its rows, and unfolding it costs no data.
///
/// A row that names a session reports that session alone; a row that only groups
/// reports the strongest mark below it. The row clicked for a window answers for that
/// window. See `docs/notes/tui-tree.md`.
pub fn rows(listed: &[Listed], folded: &BTreeSet<String>, unseen: &BTreeSet<String>) -> Vec<Row> {
    let mut nodes: Vec<Node> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut roots: Vec<usize> = Vec::new();
    for listed in listed {
        let mark = mark_of(listed, unseen);
        let mut prefix = String::new();
        let mut parent: Option<usize> = None;
        for (depth, segment) in listed.name.split('/').enumerate() {
            if depth > 0 {
                prefix.push('/');
            }
            prefix.push_str(segment);
            let at = match index.get(prefix.as_str()) {
                Some(&at) => at,
                None => {
                    let at = nodes.len();
                    nodes.push(Node {
                        name: prefix.clone(),
                        label: segment.to_string(),
                        depth,
                        children: Vec::new(),
                        session: false,
                        own: Mark::Idle,
                        under: Mark::Idle,
                    });
                    index.insert(prefix.clone(), at);
                    match parent {
                        Some(parent) => nodes[parent].children.push(at),
                        None => roots.push(at),
                    }
                    at
                }
            };
            // Every row above this one holds this session too, so a group among
            // them answers for it.
            nodes[at].under = nodes[at].under.max(mark);
            parent = Some(at);
        }
        if let Some(last) = parent {
            nodes[last].session = true;
            nodes[last].own = nodes[last].own.max(mark);
        }
    }
    // Depth first without recursion: a name is arbitrarily deep and a stack is not.
    let mut rows = Vec::with_capacity(nodes.len());
    let mut stack: Vec<usize> = roots.iter().rev().copied().collect();
    while let Some(at) = stack.pop() {
        let node = &nodes[at];
        let collapsed = !node.children.is_empty() && folded.contains(node.name.as_str());
        rows.push(Row {
            name: node.name.clone(),
            label: node.label.clone(),
            depth: node.depth,
            session: node.session,
            group: !node.children.is_empty(),
            mark: match node.session {
                true => node.own,
                false => node.under,
            },
            collapsed,
        });
        if collapsed {
            continue;
        }
        stack.extend(node.children.iter().rev().copied());
    }
    rows
}

/// The mark of one session: what it is doing, or that it stopped unseen.
///
/// # Invariant
///
/// An error keeps its own mark: the failure is the stronger fact, and it is
/// already the loudest mark in the column. See `docs/notes/tui-tree.md`.
fn mark_of(listed: &Listed, unseen: &BTreeSet<String>) -> Mark {
    let mark = Mark::of(&listed.status);
    if mark == Mark::Idle && unseen.contains(&listed.name) {
        return Mark::Stopped;
    }
    mark
}
