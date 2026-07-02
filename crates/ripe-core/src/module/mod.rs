//! The module contract: what every source/operator/output implements, plus
//! the registry that maps kind names to implementations.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::sync::Arc;

use crate::item::{Item, PortSpec, PortValue};
use crate::params::{ParamSchema, Params};

mod op_filter;
mod op_limit;
mod op_regex;
mod op_reverse;
mod op_sort;
mod op_transform;
mod op_union;
mod op_unique;
mod output;
mod source_csv;
mod source_feed;
mod source_json;

pub use op_filter::Filter;
pub use op_limit::{Limit, Tail};
pub use op_regex::RegexOp;
pub use op_reverse::Reverse;
pub use op_sort::Sort;
pub use op_transform::Transform;
pub use op_union::Union;
pub use op_unique::Unique;
pub use output::Output;
pub use source_csv::FetchCsv;
pub use source_feed::FetchFeed;
pub use source_json::FetchJson;

/// Port specs shared by the common `Items -> Items` operator shape.
pub(crate) const ITEMS_IN: &[crate::item::PortSpec] = &[crate::item::PortSpec::required(
    "in",
    crate::item::PortType::Items,
)];
pub(crate) const ITEMS_OUT: &[crate::item::PortSpec] = &[crate::item::PortSpec::required(
    "out",
    crate::item::PortType::Items,
)];

/// Everything a module may need during evaluation. Injected (rather than
/// global) so tests can pin the clock and point HTTP at a mock server.
pub struct EvalCtx {
    pub http: crate::fetch::FetchClient,
    /// Snapshot of "now", taken once per engine run.
    pub now: chrono::DateTime<chrono::Utc>,
    /// Pipe-param values for this run; the engine expands `${name}` refs
    /// before params reach a module.
    pub bindings: crate::bind::Bindings,
}

impl EvalCtx {
    pub fn new(http: crate::fetch::FetchClient) -> Self {
        Self {
            http,
            now: chrono::Utc::now(),
            bindings: crate::bind::Bindings::empty(),
        }
    }

    pub fn with_bindings(mut self, bindings: crate::bind::Bindings) -> Self {
        self.bindings = bindings;
        self
    }
}

/// Values arriving at a node's input ports. Each port holds a list because
/// variadic ports accept multiple wires; single-wire ports hold one entry.
#[derive(Debug, Clone, Default)]
pub struct Ins(BTreeMap<String, Vec<PortValue>>);

impl Ins {
    pub fn push(&mut self, port: &str, value: PortValue) {
        self.0.entry(port.to_string()).or_default().push(value);
    }

    /// The single value on `port`, if connected.
    pub fn get(&self, port: &str) -> Option<&PortValue> {
        self.0.get(port)?.first()
    }

    /// The single value on `port`; error if absent (engine guarantees
    /// required ports are wired before eval, so this is a module-logic bug).
    pub fn one(&self, port: &str) -> anyhow::Result<&PortValue> {
        self.get(port)
            .ok_or_else(|| anyhow::anyhow!("input port `{port}` is not connected"))
    }

    /// The items on `port`.
    pub fn items(&self, port: &str) -> anyhow::Result<&[Item]> {
        match self.one(port)? {
            PortValue::Items(items) => Ok(items),
            other => anyhow::bail!(
                "input port `{port}` expected items, got {:?}",
                other.port_type()
            ),
        }
    }

    /// All values on a variadic `port`, in wire order.
    pub fn all(&self, port: &str) -> &[PortValue] {
        self.0.get(port).map(Vec::as_slice).unwrap_or(&[])
    }
}

/// Values a module produced, keyed by output port name.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct Outs(pub BTreeMap<String, PortValue>);

impl Outs {
    /// The common case: a single `out` port carrying items.
    pub fn items(items: Vec<Item>) -> Self {
        Self::single(PortValue::Items(items))
    }

    /// A single value on the conventional `out` port.
    pub fn single(value: PortValue) -> Self {
        let mut map = BTreeMap::new();
        map.insert("out".to_string(), value);
        Self(map)
    }

    pub fn get(&self, port: &str) -> Option<&PortValue> {
        self.0.get(port)
    }

    /// Item count of the first `Items` output, for status displays.
    pub fn item_count(&self) -> Option<usize> {
        self.0.values().find_map(PortValue::item_count)
    }
}

/// The contract every module implements. Modules are stateless: one shared
/// instance per kind lives in the registry, and everything per-eval arrives
/// through the arguments.
#[async_trait::async_trait]
pub trait Module: Send + Sync {
    fn kind(&self) -> &'static str;
    fn inputs(&self) -> &'static [PortSpec];
    fn outputs(&self) -> &'static [PortSpec];
    fn param_schema(&self) -> ParamSchema;
    async fn eval(&self, ctx: &EvalCtx, ins: Ins, params: &Params) -> anyhow::Result<Outs>;
}

/// Maps module kind names to implementations. The palette, the loader, and
/// the engine all share this one list.
#[derive(Default)]
pub struct Registry {
    modules: HashMap<&'static str, Arc<dyn Module>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// All built-in modules.
    pub fn with_builtins() -> Self {
        let mut r = Self::new();
        r.register(Arc::new(FetchFeed));
        r.register(Arc::new(FetchJson));
        r.register(Arc::new(FetchCsv));
        r.register(Arc::new(Filter));
        r.register(Arc::new(Sort));
        r.register(Arc::new(Limit));
        r.register(Arc::new(Tail));
        r.register(Arc::new(Unique));
        r.register(Arc::new(Reverse));
        r.register(Arc::new(Union));
        r.register(Arc::new(Transform));
        r.register(Arc::new(RegexOp));
        r.register(Arc::new(Output));
        r
    }

    pub fn register(&mut self, module: Arc<dyn Module>) {
        self.modules.insert(module.kind(), module);
    }

    pub fn get(&self, kind: &str) -> Option<&Arc<dyn Module>> {
        self.modules.get(kind)
    }

    pub fn kinds(&self) -> Vec<&'static str> {
        let mut kinds: Vec<_> = self.modules.keys().copied().collect();
        kinds.sort_unstable();
        kinds
    }
}

/// Dummy modules for engine/graph tests. Compiled into the crate's test
/// builds only.
#[cfg(test)]
pub mod test_support {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::item::PortType;
    use crate::params::{FieldKind, FieldSpec};

    /// EvalCtx with a pinned clock (no HTTP mocking; operator tests don't fetch).
    pub fn ctx_at(now: chrono::DateTime<chrono::Utc>) -> EvalCtx {
        EvalCtx {
            http: crate::fetch::FetchClient::default(),
            now,
            bindings: crate::bind::Bindings::empty(),
        }
    }

    /// Items from a JSON array literal.
    pub fn items_json(v: serde_json::Value) -> Vec<Item> {
        v.as_array()
            .expect("array literal")
            .iter()
            .map(|v| Item(v.as_object().expect("object items").clone()))
            .collect()
    }

    /// Run an `Items -> Items` operator on `input` with a pinned clock.
    pub async fn run_op_at(
        module: &dyn Module,
        input: Vec<Item>,
        params: Params,
        now: chrono::DateTime<chrono::Utc>,
    ) -> anyhow::Result<Vec<Item>> {
        let mut ins = Ins::default();
        ins.push("in", PortValue::Items(input));
        let outs = module.eval(&ctx_at(now), ins, &params).await?;
        match outs.get("out") {
            Some(PortValue::Items(items)) => Ok(items.clone()),
            other => anyhow::bail!("expected items on `out`, got {other:?}"),
        }
    }

    /// As [`run_op_at`] with an arbitrary clock.
    pub async fn run_op(
        module: &dyn Module,
        input: Vec<Item>,
        params: Params,
    ) -> anyhow::Result<Vec<Item>> {
        run_op_at(module, input, params, chrono::Utc::now()).await
    }

    /// Titles of `items`, for terse assertions.
    pub fn titles(items: &[Item]) -> Vec<String> {
        items.iter().filter_map(|i| i.get_str("title")).collect()
    }

    /// Source that emits the items given in its `items` param.
    pub struct StaticSource;

    #[async_trait::async_trait]
    impl Module for StaticSource {
        fn kind(&self) -> &'static str {
            "test_source"
        }
        fn inputs(&self) -> &'static [PortSpec] {
            &[]
        }
        fn outputs(&self) -> &'static [PortSpec] {
            const OUT: &[PortSpec] = &[PortSpec::required("out", PortType::Items)];
            OUT
        }
        fn param_schema(&self) -> ParamSchema {
            ParamSchema::new(vec![FieldSpec::optional("items", "Items", FieldKind::Text)])
        }
        async fn eval(&self, _ctx: &EvalCtx, _ins: Ins, params: &Params) -> anyhow::Result<Outs> {
            let items = params
                .get("items")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_object().cloned())
                        .map(Item)
                        .collect()
                })
                .unwrap_or_default();
            Ok(Outs::items(items))
        }
    }

    /// Passthrough that counts how often it is evaluated (for cache tests).
    pub struct CountingPass(pub Arc<AtomicUsize>);

    #[async_trait::async_trait]
    impl Module for CountingPass {
        fn kind(&self) -> &'static str {
            "test_pass"
        }
        fn inputs(&self) -> &'static [PortSpec] {
            const IN: &[PortSpec] = &[PortSpec::required("in", PortType::Items)];
            IN
        }
        fn outputs(&self) -> &'static [PortSpec] {
            const OUT: &[PortSpec] = &[PortSpec::required("out", PortType::Items)];
            OUT
        }
        fn param_schema(&self) -> ParamSchema {
            ParamSchema::new(vec![FieldSpec::optional("tag", "Tag", FieldKind::Text)])
        }
        async fn eval(&self, _ctx: &EvalCtx, ins: Ins, _params: &Params) -> anyhow::Result<Outs> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(Outs::items(ins.items("in")?.to_vec()))
        }
    }

    /// Consumes a Number (for type-compat tests).
    pub struct NumberSink;

    #[async_trait::async_trait]
    impl Module for NumberSink {
        fn kind(&self) -> &'static str {
            "test_number_sink"
        }
        fn inputs(&self) -> &'static [PortSpec] {
            const IN: &[PortSpec] = &[PortSpec::required("in", PortType::Number)];
            IN
        }
        fn outputs(&self) -> &'static [PortSpec] {
            &[]
        }
        fn param_schema(&self) -> ParamSchema {
            ParamSchema::default()
        }
        async fn eval(&self, _ctx: &EvalCtx, _ins: Ins, _params: &Params) -> anyhow::Result<Outs> {
            Ok(Outs::default())
        }
    }

    /// Always fails (for error-propagation tests).
    pub struct AlwaysFails;

    #[async_trait::async_trait]
    impl Module for AlwaysFails {
        fn kind(&self) -> &'static str {
            "test_fail"
        }
        fn inputs(&self) -> &'static [PortSpec] {
            const IN: &[PortSpec] = &[PortSpec::required("in", PortType::Items)];
            IN
        }
        fn outputs(&self) -> &'static [PortSpec] {
            const OUT: &[PortSpec] = &[PortSpec::required("out", PortType::Items)];
            OUT
        }
        fn param_schema(&self) -> ParamSchema {
            ParamSchema::default()
        }
        async fn eval(&self, _ctx: &EvalCtx, _ins: Ins, _params: &Params) -> anyhow::Result<Outs> {
            anyhow::bail!("this module always fails")
        }
    }

    /// Registry with all dummies. Returns the eval counter for `test_pass`.
    pub fn test_registry() -> (Registry, Arc<AtomicUsize>) {
        let counter = Arc::new(AtomicUsize::new(0));
        let mut r = Registry::new();
        r.register(Arc::new(StaticSource));
        r.register(Arc::new(CountingPass(counter.clone())));
        r.register(Arc::new(NumberSink));
        r.register(Arc::new(AlwaysFails));
        (r, counter)
    }
}
