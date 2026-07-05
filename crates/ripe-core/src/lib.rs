//! ripe-core: data model, graph, engine, and modules for ripe.
//!
//! UI-free by design: both the TUI (`ripe`) and the headless runner
//! (`ripe-run`) sit on top of this crate.

pub mod bind;
pub mod engine;
pub mod expr;
pub mod fetch;
pub mod format;
pub mod graph;
pub mod item;
pub mod logging;
pub mod module;
pub mod params;
pub mod persist;
pub mod preview;

pub use bind::{Bindings, PipeParam, PipeParamKind};
pub use engine::{Engine, EngineConfig, EvalCache, EvalReport, NodeStatus};
pub use format::{Format, emit};
pub use graph::{Edge, Node, NodeId, Pipe};
pub use item::{Item, PortSpec, PortType, PortValue};
pub use module::{EvalCtx, Module, Registry};
pub use params::{ParamSchema, Params};
pub use persist::{Loaded, PersistError, load_pipe, save_pipe};
pub use preview::Preview;
