//! ripe-core: data model, graph, engine, and modules for ripe.
//!
//! UI-free by design: both the TUI (`ripe`) and the headless runner
//! (`ripe-run`) sit on top of this crate.

pub mod engine;
pub mod expr;
pub mod fetch;
pub mod graph;
pub mod item;
pub mod logging;
pub mod module;
pub mod params;

pub use engine::{Engine, EngineConfig, EvalCache, EvalReport, NodeStatus};
pub use graph::{Edge, Node, NodeId, Pipe};
pub use item::{Item, PortSpec, PortType, PortValue};
pub use module::{EvalCtx, Module, Registry};
pub use params::{ParamSchema, Params};
