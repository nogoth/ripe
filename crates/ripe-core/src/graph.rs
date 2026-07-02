//! The pipe graph: nodes, edges, and structural validation.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use crate::module::Registry;
use crate::params::Params;

/// Stable identifier for a node within one pipe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct NodeId(pub u64);

impl std::fmt::Display for NodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "#{}", self.0)
    }
}

/// One end of an edge: a named port on a node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortRef {
    pub node: NodeId,
    pub port: String,
}

impl PortRef {
    pub fn new(node: NodeId, port: impl Into<String>) -> Self {
        Self {
            node,
            port: port.into(),
        }
    }
}

/// A wire from an output port to an input port.
///
/// Edge order in `Pipe::edges` is meaningful: it defines the order of
/// values arriving at a variadic port (and the memo-key ordering).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edge {
    pub from: PortRef,
    pub to: PortRef,
}

/// A module instance in the graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: NodeId,
    pub kind: String,
    #[serde(default)]
    pub params: Params,
}

/// Current on-disk format version (see PLAN.md M6 for migrations).
pub const PIPE_FORMAT_VERSION: u32 = 1;

/// A pipe: a DAG of modules. Layout is computed at render time and never
/// stored here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pipe {
    pub version: u32,
    pub name: String,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    next_id: u64,
}

impl Pipe {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            version: PIPE_FORMAT_VERSION,
            name: name.into(),
            nodes: Vec::new(),
            edges: Vec::new(),
            next_id: 1,
        }
    }

    pub fn add_node(&mut self, kind: impl Into<String>, params: Params) -> NodeId {
        let id = NodeId(self.next_id);
        self.next_id += 1;
        self.nodes.push(Node {
            id,
            kind: kind.into(),
            params,
        });
        id
    }

    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }

    pub fn node_mut(&mut self, id: NodeId) -> Option<&mut Node> {
        self.nodes.iter_mut().find(|n| n.id == id)
    }

    /// Add a wire. Structural legality (ports exist, types fit, no cycle)
    /// is checked by [`Pipe::validate`], not here.
    pub fn connect(
        &mut self,
        from: NodeId,
        from_port: impl Into<String>,
        to: NodeId,
        to_port: impl Into<String>,
    ) {
        self.edges.push(Edge {
            from: PortRef::new(from, from_port),
            to: PortRef::new(to, to_port),
        });
    }

    pub fn remove_node(&mut self, id: NodeId) {
        self.nodes.retain(|n| n.id != id);
        self.edges.retain(|e| e.from.node != id && e.to.node != id);
    }

    /// Incoming edges for `node`, in `edges` order.
    pub fn edges_into(&self, node: NodeId) -> impl Iterator<Item = &Edge> {
        self.edges.iter().filter(move |e| e.to.node == node)
    }

    /// Structural validation: everything that makes a pipe unloadable or
    /// unevaluable *regardless of editing state*. A missing required input
    /// is deliberately NOT an error here — while editing, graphs are
    /// transiently incomplete; the engine reports such nodes as `Unready`.
    pub fn validate(&self, registry: &Registry) -> Vec<String> {
        let mut errors = Vec::new();
        let by_id: HashMap<NodeId, &Node> = self.nodes.iter().map(|n| (n.id, n)).collect();
        if by_id.len() != self.nodes.len() {
            errors.push("duplicate node ids".to_string());
        }

        for node in &self.nodes {
            if registry.get(&node.kind).is_none() {
                errors.push(format!(
                    "node {}: unknown module kind `{}`",
                    node.id, node.kind
                ));
            }
        }

        let mut fan_in: BTreeMap<(NodeId, &str), usize> = BTreeMap::new();
        for edge in &self.edges {
            let (Some(from_node), Some(to_node)) =
                (by_id.get(&edge.from.node), by_id.get(&edge.to.node))
            else {
                errors.push(format!(
                    "dangling edge {} -> {}: node missing",
                    edge.from.node, edge.to.node
                ));
                continue;
            };
            let (Some(from_mod), Some(to_mod)) =
                (registry.get(&from_node.kind), registry.get(&to_node.kind))
            else {
                continue; // unknown kind already reported
            };
            let from_spec = from_mod.outputs().iter().find(|s| s.name == edge.from.port);
            let to_spec = to_mod.inputs().iter().find(|s| s.name == edge.to.port);
            let Some(from_spec) = from_spec else {
                errors.push(format!(
                    "edge from {}:{}: `{}` has no such output port",
                    edge.from.node, edge.from.port, from_node.kind
                ));
                continue;
            };
            let Some(to_spec) = to_spec else {
                errors.push(format!(
                    "edge into {}:{}: `{}` has no such input port",
                    edge.to.node, edge.to.port, to_node.kind
                ));
                continue;
            };
            if !to_spec.ty.accepts(from_spec.ty) {
                errors.push(format!(
                    "type mismatch: {}:{} ({:?}) cannot feed {}:{} ({:?})",
                    edge.from.node,
                    edge.from.port,
                    from_spec.ty,
                    edge.to.node,
                    edge.to.port,
                    to_spec.ty
                ));
            }
            if !to_spec.variadic {
                let count = fan_in.entry((edge.to.node, to_spec.name)).or_insert(0);
                *count += 1;
                if *count == 2 {
                    errors.push(format!(
                        "port {}:{} is not variadic but has multiple incoming edges",
                        edge.to.node, edge.to.port
                    ));
                }
            }
        }

        if let Err(cycle_node) = self.topo_order() {
            errors.push(format!("pipe contains a cycle (through node {cycle_node})"));
        }

        errors
    }

    /// Node ids in dependency order. `Err` carries a node on a cycle.
    pub fn topo_order(&self) -> Result<Vec<NodeId>, NodeId> {
        let mut g = petgraph::graph::DiGraph::<NodeId, ()>::new();
        let mut idx = HashMap::new();
        for node in &self.nodes {
            idx.insert(node.id, g.add_node(node.id));
        }
        for edge in &self.edges {
            if let (Some(&a), Some(&b)) = (idx.get(&edge.from.node), idx.get(&edge.to.node)) {
                g.add_edge(a, b, ());
            }
        }
        petgraph::algo::toposort(&g, None)
            .map(|order| order.into_iter().map(|i| g[i]).collect())
            .map_err(|cycle| g[cycle.node_id()])
    }

    /// Layer index per node: sources at 0, every other node one past its
    /// deepest upstream. Drives both eval waves and (later) auto-layout.
    pub fn layers(&self) -> Result<BTreeMap<NodeId, usize>, NodeId> {
        let order = self.topo_order()?;
        let mut layers: BTreeMap<NodeId, usize> = BTreeMap::new();
        for id in order {
            let layer = self
                .edges_into(id)
                .filter_map(|e| layers.get(&e.from.node))
                .map(|l| l + 1)
                .max()
                .unwrap_or(0);
            layers.insert(id, layer);
        }
        Ok(layers)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::module::test_support::test_registry;

    #[test]
    fn topo_and_layers() {
        let mut pipe = Pipe::new("t");
        let src = pipe.add_node("test_source", Params::new());
        let a = pipe.add_node("test_pass", Params::new());
        let b = pipe.add_node("test_pass", Params::new());
        pipe.connect(src, "out", a, "in");
        pipe.connect(a, "out", b, "in");
        let order = pipe.topo_order().unwrap();
        assert_eq!(order, vec![src, a, b]);
        let layers = pipe.layers().unwrap();
        assert_eq!(layers[&src], 0);
        assert_eq!(layers[&a], 1);
        assert_eq!(layers[&b], 2);
    }

    #[test]
    fn cycle_is_rejected() {
        let (registry, _) = test_registry();
        let mut pipe = Pipe::new("t");
        let a = pipe.add_node("test_pass", Params::new());
        let b = pipe.add_node("test_pass", Params::new());
        pipe.connect(a, "out", b, "in");
        pipe.connect(b, "out", a, "in");
        assert!(pipe.topo_order().is_err());
        let errors = pipe.validate(&registry);
        assert!(errors.iter().any(|e| e.contains("cycle")), "{errors:?}");
    }

    #[test]
    fn unknown_kind_dangling_edge_and_bad_port_are_errors() {
        let (registry, _) = test_registry();
        let mut pipe = Pipe::new("t");
        let a = pipe.add_node("no_such_kind", Params::new());
        let b = pipe.add_node("test_pass", Params::new());
        pipe.connect(b, "out", NodeId(99), "in");
        pipe.connect(b, "nope", b, "in"); // self-edge is also a cycle, but port error fires first
        pipe.connect(a, "out", b, "in");
        let errors = pipe.validate(&registry);
        assert!(
            errors.iter().any(|e| e.contains("unknown module kind")),
            "{errors:?}"
        );
        assert!(
            errors.iter().any(|e| e.contains("dangling edge")),
            "{errors:?}"
        );
        assert!(
            errors.iter().any(|e| e.contains("no such output port")),
            "{errors:?}"
        );
    }

    #[test]
    fn type_mismatch_and_fan_in_are_errors() {
        let (registry, _) = test_registry();
        let mut pipe = Pipe::new("t");
        let s1 = pipe.add_node("test_source", Params::new());
        let s2 = pipe.add_node("test_source", Params::new());
        let sink = pipe.add_node("test_number_sink", Params::new());
        let pass = pipe.add_node("test_pass", Params::new());
        pipe.connect(s1, "out", sink, "in"); // Items -> Number: mismatch
        pipe.connect(s1, "out", pass, "in");
        pipe.connect(s2, "out", pass, "in"); // second edge into non-variadic port
        let errors = pipe.validate(&registry);
        assert!(
            errors.iter().any(|e| e.contains("type mismatch")),
            "{errors:?}"
        );
        assert!(
            errors.iter().any(|e| e.contains("not variadic")),
            "{errors:?}"
        );
    }

    #[test]
    fn pipe_serde_round_trip() {
        let mut pipe = Pipe::new("round-trip");
        let s = pipe.add_node(
            "test_source",
            Params::new().with("items", serde_json::json!([])),
        );
        let p = pipe.add_node("test_pass", Params::new());
        pipe.connect(s, "out", p, "in");
        let json = serde_json::to_string_pretty(&pipe).unwrap();
        let back: Pipe = serde_json::from_str(&json).unwrap();
        assert_eq!(back, pipe);
        // next_id must survive so future adds don't collide.
        let mut back = back;
        assert_eq!(back.add_node("test_pass", Params::new()), NodeId(3));
    }
}
