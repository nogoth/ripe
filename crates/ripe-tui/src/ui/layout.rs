//! Pure auto-layout: turn a [`Pipe`]'s graph into grid slots. UI-free by
//! design so it can be unit-tested without a terminal — it computes only
//! `(row, column)` positions; pixels and wires live in [`super::canvas`].
//!
//! # Stability contract
//!
//! The #1 project risk (PLAN.md) is layout *instability*: editing one branch
//! must not reshuffle unrelated ones. This module guarantees:
//!
//! - **Rows** come straight from [`Pipe::layers`] (longest-path layering,
//!   sources at row 0). A node's row depends only on its own longest path
//!   from a source, so it never moves because of a sibling branch.
//! - **Columns** are assigned per row by *barycenter*: a node's column is the
//!   rank of the mean of its already-placed upstream columns, among its
//!   row-mates, tie-broken by [`NodeId`]. Because a node's column depends
//!   only on the columns of its own ancestors — never on unrelated nodes —
//!   inserting or deleting a node on one branch cannot change another
//!   branch's columns, as long as the edit does not reorder barycenters
//!   *within* a shared row (it can't, for disjoint branches).
//!
//! Both axes are deterministic: identical pipes always produce identical
//! layouts (see the `deterministic` and `stability` tests).

use std::collections::{BTreeMap, HashMap};

use ripe_core::{NodeId, Pipe};

/// A node's position in the layout grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slot {
    /// Layer index: sources at 0, deeper nodes below.
    pub row: usize,
    /// Rank within the row, left to right, 0-based.
    pub col: usize,
}

/// Grid placement for every node in a pipe.
#[derive(Debug, Clone, Default)]
pub struct Layout {
    slots: BTreeMap<NodeId, Slot>,
    rows: usize,
}

impl Layout {
    /// Place every node. A cyclic pipe (which [`Pipe::layers`] rejects) yields
    /// an empty layout — the canvas simply draws nothing, and validation
    /// surfaces the cycle elsewhere.
    pub fn compute(pipe: &Pipe) -> Self {
        let Ok(layers) = pipe.layers() else {
            return Layout::default();
        };
        let row_count = layers.values().copied().max().map_or(0, |m| m + 1);

        // Bucket nodes by row so we can rank each row independently.
        let mut by_row: Vec<Vec<NodeId>> = vec![Vec::new(); row_count];
        for (&id, &row) in &layers {
            by_row[row].push(id);
        }

        // Assign columns top-down: every node's upstreams sit in a strictly
        // lower row (layer = max(upstream layers) + 1), so they are already
        // placed when we reach it and their barycenter is well-defined.
        let mut col: HashMap<NodeId, usize> = HashMap::new();
        for row in &by_row {
            let mut ranked: Vec<(f64, NodeId)> = row
                .iter()
                .map(|&id| (barycenter(pipe, id, &col), id))
                .collect();
            // Barycenter first, NodeId as a stable tie-break.
            ranked.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
            for (c, (_, id)) in ranked.into_iter().enumerate() {
                col.insert(id, c);
            }
        }

        let slots = layers
            .iter()
            .map(|(&id, &row)| (id, Slot { row, col: col[&id] }))
            .collect();
        Layout {
            slots,
            rows: row_count,
        }
    }

    /// The slot for `id`, if it is in this layout.
    pub fn slot(&self, id: NodeId) -> Option<Slot> {
        self.slots.get(&id).copied()
    }

    /// Every `(node, slot)` pair, in ascending [`NodeId`] order.
    pub fn iter(&self) -> impl Iterator<Item = (NodeId, Slot)> + '_ {
        self.slots.iter().map(|(&id, &slot)| (id, slot))
    }

    /// Number of rows (layers) in the layout.
    pub fn rows(&self) -> usize {
        self.rows
    }
}

/// Mean of `id`'s already-placed upstream columns, or `0.0` for a source
/// (which sorts purely by its [`NodeId`] tie-break).
fn barycenter(pipe: &Pipe, id: NodeId, placed: &HashMap<NodeId, usize>) -> f64 {
    let cols: Vec<usize> = pipe
        .edges_into(id)
        .filter_map(|e| placed.get(&e.from.node).copied())
        .collect();
    if cols.is_empty() {
        0.0
    } else {
        cols.iter().sum::<usize>() as f64 / cols.len() as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ripe_core::{Params, Pipe, Registry, load_pipe};
    use std::path::Path;

    /// Two independent chains, A (ids 1-3) and B (ids 4-6), each
    /// `source -> pass -> pass`. A's ids sort before B's, so A takes column 0.
    fn two_branches() -> (Pipe, [NodeId; 3], [NodeId; 3]) {
        let mut pipe = Pipe::new("two-branches");
        let a0 = pipe.add_node("fetch_feed", Params::new().with("url", "https://a.example"));
        let a1 = pipe.add_node("filter", Params::new());
        let a2 = pipe.add_node("sort", Params::new().with("by", "title"));
        pipe.connect(a0, "out", a1, "in");
        pipe.connect(a1, "out", a2, "in");
        let b0 = pipe.add_node("fetch_feed", Params::new().with("url", "https://b.example"));
        let b1 = pipe.add_node("filter", Params::new());
        let b2 = pipe.add_node("sort", Params::new().with("by", "title"));
        pipe.connect(b0, "out", b1, "in");
        pipe.connect(b1, "out", b2, "in");
        (pipe, [a0, a1, a2], [b0, b1, b2])
    }

    #[test]
    fn deterministic_across_runs() {
        let (pipe, _, _) = two_branches();
        let first = Layout::compute(&pipe);
        let second = Layout::compute(&pipe);
        assert_eq!(first.slots, second.slots);
    }

    #[test]
    fn branches_occupy_distinct_columns() {
        let (pipe, a, b) = two_branches();
        for id in a {
            assert_eq!(Layout::compute(&pipe).slot(id).unwrap().col, 0);
        }
        for id in b {
            assert_eq!(Layout::compute(&pipe).slot(id).unwrap().col, 1);
        }
    }

    /// The hard requirement: growing branch A must not move any branch-B node.
    #[test]
    fn inserting_on_one_branch_leaves_the_other_fixed() {
        let (mut pipe, a, b) = two_branches();
        let before = Layout::compute(&pipe);
        let b_cols: Vec<usize> = b.iter().map(|&id| before.slot(id).unwrap().col).collect();

        // Splice a new node into branch A: a1 -> new -> a2. This lengthens A
        // (a2 drops a row) but touches nothing in B.
        let inserted = pipe.add_node("regex", Params::new().with("pattern", "x"));
        pipe.edges
            .retain(|e| !(e.from.node == a[1] && e.to.node == a[2]));
        pipe.connect(a[1], "out", inserted, "in");
        pipe.connect(inserted, "out", a[2], "in");

        let after = Layout::compute(&pipe);
        for (&id, &want) in b.iter().zip(&b_cols) {
            let slot = after.slot(id).unwrap();
            assert_eq!(slot.col, want, "branch-B node {id} changed column");
        }
        // And branch A really did grow — proof the edit took effect.
        assert!(after.slot(a[2]).unwrap().row > before.slot(a[2]).unwrap().row);
        assert_eq!(after.slot(inserted).unwrap().col, 0);
    }

    #[test]
    fn kitchen_sink_lays_out_without_overlaps() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/kitchen_sink.pipe");
        let loaded = load_pipe(&path, &Registry::with_builtins()).expect("load kitchen sink");
        let layout = Layout::compute(&loaded.pipe);

        assert_eq!(
            layout.iter().count(),
            loaded.pipe.nodes.len(),
            "every node placed"
        );
        let mut seen = std::collections::HashSet::new();
        for (id, slot) in layout.iter() {
            assert!(
                seen.insert((slot.row, slot.col)),
                "node {id} collides at row {}, col {}",
                slot.row,
                slot.col
            );
        }
    }
}
