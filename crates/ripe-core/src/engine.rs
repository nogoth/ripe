//! Async pipe evaluation with per-node memoization.
//!
//! Nodes evaluate in dependency layers; independent branches within a layer
//! run concurrently. Each node's output is cached under a key derived from
//! its kind, params, and upstream output hashes, so editing one node only
//! re-evaluates its descendants.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;

use crate::graph::{NodeId, Pipe};
use crate::item::content_hash;
use crate::module::{EvalCtx, Ins, Outs, Registry};

/// Engine tuning knobs.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Sources have no upstreams, so `(kind, params)` alone would cache them
    /// forever. Their memo key includes `now / source_ttl` as a bucket; when
    /// the bucket rolls over, the source re-fetches (HTTP conditional
    /// requests keep that cheap).
    pub source_ttl: Duration,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            source_ttl: Duration::from_secs(300),
        }
    }
}

/// Why (or whether) a node produced output this run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeStatus {
    /// Evaluated fresh this run.
    Ok,
    /// Memo key matched; cached output reused.
    Cached,
    /// Evaluation failed (message in `NodeReport::error`).
    Err,
    /// Not evaluable: a required input is unwired, or an upstream failed.
    /// Never a whole-pipe error — editing produces transiently incomplete
    /// graphs and the rest of the pipe should keep working.
    Unready,
}

/// Per-node outcome of one engine run.
#[derive(Debug, Clone)]
pub struct NodeReport {
    pub status: NodeStatus,
    pub error: Option<String>,
    pub duration: Duration,
    pub item_count: Option<usize>,
}

/// Outcome of one engine run, keyed by node.
#[derive(Debug, Clone, Default)]
pub struct EvalReport {
    pub nodes: BTreeMap<NodeId, NodeReport>,
}

impl EvalReport {
    pub fn status(&self, id: NodeId) -> Option<&NodeStatus> {
        self.nodes.get(&id).map(|r| &r.status)
    }
}

struct CacheEntry {
    key: u64,
    outs: Arc<Outs>,
    /// Content hash of `outs`, folded into downstream memo keys.
    out_hash: u64,
}

/// Memoized node outputs. Owned by the caller (the TUI keeps one per open
/// pipe) and passed into each run; entries survive across runs until their
/// key changes.
#[derive(Default)]
pub struct EvalCache {
    entries: HashMap<NodeId, CacheEntry>,
}

impl EvalCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// The most recent output of `node`, if it has ever evaluated cleanly.
    pub fn output(&self, node: NodeId) -> Option<Arc<Outs>> {
        self.entries.get(&node).map(|e| e.outs.clone())
    }
}

/// Everything hashed into a node's memo key. Serialized to canonical JSON,
/// then hashed (see `item::content_hash`).
#[derive(Serialize)]
struct MemoKey<'a> {
    kind: &'a str,
    params: &'a crate::params::Params,
    /// Upstream output hashes in input-port order (port name, then wire
    /// order). Order matters: sorting would conflate different wirings.
    upstream: &'a [u64],
    /// `Some(now / source_ttl)` for source nodes, `None` otherwise.
    ttl_bucket: Option<i64>,
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("pipe is invalid:\n{}", .0.join("\n"))]
    Invalid(Vec<String>),
}

pub struct Engine {
    pub registry: Arc<Registry>,
    pub config: EngineConfig,
}

impl Engine {
    pub fn new(registry: Arc<Registry>) -> Self {
        Self {
            registry,
            config: EngineConfig::default(),
        }
    }

    /// Evaluate the whole pipe. Structural problems (cycles, unknown kinds,
    /// bad ports, type mismatches) abort the run; per-node problems (fetch
    /// failures, unwired inputs) land in the report instead.
    pub async fn eval(
        &self,
        pipe: &Pipe,
        cache: &mut EvalCache,
        ctx: &EvalCtx,
    ) -> Result<EvalReport, EngineError> {
        let errors = pipe.validate(&self.registry);
        if !errors.is_empty() {
            return Err(EngineError::Invalid(errors));
        }
        let layers = pipe.layers().expect("validate() rejects cycles");
        let max_layer = layers.values().copied().max().unwrap_or(0);

        let mut report = EvalReport::default();
        for layer in 0..=max_layer {
            let wave: Vec<NodeId> = layers
                .iter()
                .filter(|&(_, l)| *l == layer)
                .map(|(id, _)| *id)
                .collect();
            self.eval_wave(pipe, &wave, cache, ctx, &mut report).await;
        }
        Ok(report)
    }

    /// Evaluate one dependency layer; nodes in a layer share no edges, so
    /// they run concurrently.
    async fn eval_wave(
        &self,
        pipe: &Pipe,
        wave: &[NodeId],
        cache: &mut EvalCache,
        ctx: &EvalCtx,
        report: &mut EvalReport,
    ) {
        // Phase 1 (sync): decide each node's fate — Unready, Cached, or run.
        let mut to_run: Vec<(NodeId, u64, Ins, crate::params::Params)> = Vec::new();
        for &id in wave {
            let node = pipe.node(id).expect("layer ids come from the pipe");
            let module = self.registry.get(&node.kind).expect("validated");

            match self.gather_inputs(pipe, id, cache, report) {
                Err(reason) => {
                    report.nodes.insert(
                        id,
                        NodeReport {
                            status: NodeStatus::Unready,
                            error: Some(reason),
                            duration: Duration::ZERO,
                            item_count: None,
                        },
                    );
                }
                Ok((ins, upstream_hashes)) => {
                    // Expand ${name} pipe params first: the memo key must
                    // hash the *resolved* params, so a `--param` change
                    // invalidates exactly the nodes that reference it.
                    let params = match crate::bind::interpolate_params(
                        &node.params,
                        &module.param_schema(),
                        &ctx.bindings,
                    ) {
                        Ok(params) => params,
                        Err(e) => {
                            report.nodes.insert(
                                id,
                                NodeReport {
                                    status: NodeStatus::Err,
                                    error: Some(e),
                                    duration: Duration::ZERO,
                                    item_count: None,
                                },
                            );
                            continue;
                        }
                    };
                    let is_source = module.inputs().is_empty();
                    let ttl_bucket = is_source.then(|| {
                        ctx.now.timestamp() / self.config.source_ttl.as_secs().max(1) as i64
                    });
                    let key = content_hash(&MemoKey {
                        kind: &node.kind,
                        params: &params,
                        upstream: &upstream_hashes,
                        ttl_bucket,
                    });
                    if cache.entries.get(&id).is_some_and(|e| e.key == key) {
                        let outs = cache.entries[&id].outs.clone();
                        report.nodes.insert(
                            id,
                            NodeReport {
                                status: NodeStatus::Cached,
                                error: None,
                                duration: Duration::ZERO,
                                item_count: outs.item_count(),
                            },
                        );
                    } else {
                        to_run.push((id, key, ins, params));
                    }
                }
            }
        }

        // Phase 2 (async): run the remaining nodes concurrently.
        let futures = to_run.into_iter().map(|(id, key, ins, params)| {
            let node = pipe.node(id).expect("checked above");
            let module = self.registry.get(&node.kind).expect("validated").clone();
            async move {
                let started = std::time::Instant::now();
                let result = module.eval(ctx, ins, &params).await;
                (id, key, started.elapsed(), result)
            }
        });
        for (id, key, duration, result) in futures::future::join_all(futures).await {
            match result {
                Ok(outs) => {
                    let out_hash = content_hash(&outs);
                    let item_count = outs.item_count();
                    cache.entries.insert(
                        id,
                        CacheEntry {
                            key,
                            outs: Arc::new(outs),
                            out_hash,
                        },
                    );
                    report.nodes.insert(
                        id,
                        NodeReport {
                            status: NodeStatus::Ok,
                            error: None,
                            duration,
                            item_count,
                        },
                    );
                }
                Err(e) => {
                    cache.entries.remove(&id);
                    report.nodes.insert(
                        id,
                        NodeReport {
                            status: NodeStatus::Err,
                            error: Some(format!("{e:#}")),
                            duration,
                            item_count: None,
                        },
                    );
                }
            }
        }
    }

    /// Collect the values arriving at `id`'s input ports, coerced to the
    /// port types, plus their hashes in port order for the memo key.
    /// `Err(reason)` means the node is Unready.
    fn gather_inputs(
        &self,
        pipe: &Pipe,
        id: NodeId,
        cache: &EvalCache,
        report: &EvalReport,
    ) -> Result<(Ins, Vec<u64>), String> {
        let node = pipe.node(id).expect("caller checked");
        let module = self.registry.get(&node.kind).expect("validated");
        let mut ins = Ins::default();
        let mut hashes = Vec::new();
        for spec in module.inputs() {
            let edges: Vec<_> = pipe
                .edges_into(id)
                .filter(|e| e.to.port == spec.name)
                .collect();
            if edges.is_empty() {
                if spec.required {
                    return Err(format!("required input `{}` is not connected", spec.name));
                }
                continue;
            }
            for edge in edges {
                let upstream_ok = matches!(
                    report.status(edge.from.node),
                    Some(NodeStatus::Ok | NodeStatus::Cached)
                );
                if !upstream_ok {
                    return Err(format!(
                        "upstream {} did not produce output",
                        edge.from.node
                    ));
                }
                let entry = cache
                    .entries
                    .get(&edge.from.node)
                    .ok_or_else(|| format!("upstream {} has no cached output", edge.from.node))?;
                let value = entry.outs.get(&edge.from.port).ok_or_else(|| {
                    format!(
                        "upstream {} has no output port `{}`",
                        edge.from.node, edge.from.port
                    )
                })?;
                let coerced = value
                    .coerce_to(spec.ty)
                    .map_err(|e| format!("input `{}`: {e}", spec.name))?;
                ins.push(spec.name, coerced);
                hashes.push(entry.out_hash);
            }
        }
        Ok((ins, hashes))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use serde_json::json;

    use super::*;
    use crate::fetch::FetchClient;
    use crate::module::test_support::test_registry;
    use crate::params::Params;

    fn ctx_at(now: chrono::DateTime<chrono::Utc>) -> EvalCtx {
        EvalCtx {
            http: FetchClient::default(),
            now,
            bindings: crate::bind::Bindings::empty(),
        }
    }

    fn ctx() -> EvalCtx {
        ctx_at(chrono::Utc::now())
    }

    fn items_param() -> Params {
        Params::new().with("items", json!([{"title": "a"}, {"title": "b"}]))
    }

    /// source -> pass -> pass
    fn chain() -> (Pipe, NodeId, NodeId, NodeId) {
        let mut pipe = Pipe::new("chain");
        let src = pipe.add_node("test_source", items_param());
        let a = pipe.add_node("test_pass", Params::new());
        let b = pipe.add_node("test_pass", Params::new().with("tag", "second"));
        pipe.connect(src, "out", a, "in");
        pipe.connect(a, "out", b, "in");
        (pipe, src, a, b)
    }

    #[tokio::test]
    async fn passthrough_pipe_evaluates() {
        let (registry, _) = test_registry();
        let engine = Engine::new(Arc::new(registry));
        let (pipe, src, a, b) = chain();
        let mut cache = EvalCache::new();
        let report = engine.eval(&pipe, &mut cache, &ctx()).await.unwrap();
        for id in [src, a, b] {
            assert_eq!(report.status(id), Some(&NodeStatus::Ok), "{id}");
            assert_eq!(report.nodes[&id].item_count, Some(2));
        }
        let out = cache.output(b).unwrap();
        assert_eq!(out.item_count(), Some(2));
    }

    #[tokio::test]
    async fn second_run_is_fully_cached() {
        let (registry, counter) = test_registry();
        let engine = Engine::new(Arc::new(registry));
        let (pipe, src, a, b) = chain();
        let mut cache = EvalCache::new();
        let now = chrono::Utc::now();
        engine.eval(&pipe, &mut cache, &ctx_at(now)).await.unwrap();
        assert_eq!(counter.load(Ordering::SeqCst), 2);
        let report = engine.eval(&pipe, &mut cache, &ctx_at(now)).await.unwrap();
        assert_eq!(counter.load(Ordering::SeqCst), 2, "no re-evals expected");
        for id in [src, a, b] {
            assert_eq!(report.status(id), Some(&NodeStatus::Cached), "{id}");
        }
    }

    #[tokio::test]
    async fn param_edit_invalidates_only_descendants() {
        let (registry, counter) = test_registry();
        let engine = Engine::new(Arc::new(registry));
        let (mut pipe, src, a, b) = chain();
        let mut cache = EvalCache::new();
        let now = chrono::Utc::now();
        engine.eval(&pipe, &mut cache, &ctx_at(now)).await.unwrap();
        counter.store(0, Ordering::SeqCst);

        // Editing the *last* node re-evaluates it alone.
        pipe.node_mut(b).unwrap().params.set("tag", "edited");
        let report = engine.eval(&pipe, &mut cache, &ctx_at(now)).await.unwrap();
        assert_eq!(report.status(src), Some(&NodeStatus::Cached));
        assert_eq!(report.status(a), Some(&NodeStatus::Cached));
        assert_eq!(report.status(b), Some(&NodeStatus::Ok));
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn source_ttl_bucket_rolls_over() {
        let (registry, _) = test_registry();
        let engine = Engine::new(Arc::new(registry));
        let (pipe, src, _, _) = chain();
        let mut cache = EvalCache::new();
        let now = chrono::Utc::now();
        engine.eval(&pipe, &mut cache, &ctx_at(now)).await.unwrap();

        let later = now
            + chrono::Duration::from_std(engine.config.source_ttl).unwrap()
            + chrono::Duration::seconds(1);
        let report = engine
            .eval(&pipe, &mut cache, &ctx_at(later))
            .await
            .unwrap();
        assert_eq!(
            report.status(src),
            Some(&NodeStatus::Ok),
            "source must re-run after TTL"
        );
    }

    #[tokio::test]
    async fn unwired_required_input_is_unready_not_fatal() {
        let (registry, _) = test_registry();
        let engine = Engine::new(Arc::new(registry));
        let mut pipe = Pipe::new("partial");
        let src = pipe.add_node("test_source", items_param());
        let lonely = pipe.add_node("test_pass", Params::new()); // never wired
        let mut cache = EvalCache::new();
        let report = engine.eval(&pipe, &mut cache, &ctx()).await.unwrap();
        assert_eq!(report.status(src), Some(&NodeStatus::Ok));
        assert_eq!(report.status(lonely), Some(&NodeStatus::Unready));
        assert!(
            report.nodes[&lonely]
                .error
                .as_deref()
                .unwrap()
                .contains("not connected")
        );
    }

    #[tokio::test]
    async fn failure_marks_downstream_unready() {
        let (registry, _) = test_registry();
        let engine = Engine::new(Arc::new(registry));
        let mut pipe = Pipe::new("failing");
        let src = pipe.add_node("test_source", items_param());
        let boom = pipe.add_node("test_fail", Params::new());
        let after = pipe.add_node("test_pass", Params::new());
        pipe.connect(src, "out", boom, "in");
        pipe.connect(boom, "out", after, "in");
        let mut cache = EvalCache::new();
        let report = engine.eval(&pipe, &mut cache, &ctx()).await.unwrap();
        assert_eq!(report.status(boom), Some(&NodeStatus::Err));
        assert!(
            report.nodes[&boom]
                .error
                .as_deref()
                .unwrap()
                .contains("always fails")
        );
        assert_eq!(report.status(after), Some(&NodeStatus::Unready));
    }

    #[tokio::test]
    async fn invalid_pipe_is_rejected() {
        let (registry, _) = test_registry();
        let engine = Engine::new(Arc::new(registry));
        let mut pipe = Pipe::new("cyclic");
        let a = pipe.add_node("test_pass", Params::new());
        let b = pipe.add_node("test_pass", Params::new());
        pipe.connect(a, "out", b, "in");
        pipe.connect(b, "out", a, "in");
        let mut cache = EvalCache::new();
        let err = engine.eval(&pipe, &mut cache, &ctx()).await.unwrap_err();
        assert!(err.to_string().contains("cycle"), "{err}");
    }

    // --- M5: pipe params -------------------------------------------------

    use crate::bind::{Bindings, PipeParam, PipeParamKind};

    #[tokio::test]
    async fn pipe_param_binds_fetch_url_at_run_time() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/feed"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"<?xml version="1.0"?><rss version="2.0"><channel><title>c</title>
                <item><title>bound</title></item></channel></rss>"#,
            ))
            .mount(&server)
            .await;

        let mut pipe = Pipe::new("param-bound");
        pipe.params.push(PipeParam::new("url", PipeParamKind::Url));
        let fetch = pipe.add_node("fetch_feed", Params::new().with("url", "${url}"));
        let out = pipe.add_node("output", Params::new());
        pipe.connect(fetch, "out", out, "in");

        let bindings = Bindings::resolve(
            &pipe.params,
            &[("url".to_string(), format!("{}/feed", server.uri()))],
        )
        .unwrap();
        let engine = Engine::new(Arc::new(crate::module::Registry::with_builtins()));
        let mut cache = EvalCache::new();
        let ctx = EvalCtx::new(FetchClient::default()).with_bindings(bindings);
        let report = engine.eval(&pipe, &mut cache, &ctx).await.unwrap();
        assert_eq!(report.status(fetch), Some(&NodeStatus::Ok), "{report:?}");
        let stream = cache.output(pipe.output_node().unwrap()).unwrap();
        assert_eq!(stream.item_count(), Some(1));
    }

    #[tokio::test]
    async fn binding_change_invalidates_only_referencing_nodes() {
        let (registry, _) = test_registry();
        let engine = Engine::new(Arc::new(registry));
        let mut pipe = Pipe::new("bound");
        pipe.params
            .push(PipeParam::new("tag", PipeParamKind::Text).with_default("a"));
        let src = pipe.add_node("test_source", items_param());
        let pass = pipe.add_node("test_pass", Params::new().with("tag", "${tag}"));
        pipe.connect(src, "out", pass, "in");

        let now = chrono::Utc::now();
        let mut cache = EvalCache::new();
        let ctx1 = ctx_at(now).with_bindings(Bindings::resolve(&pipe.params, &[]).unwrap());
        engine.eval(&pipe, &mut cache, &ctx1).await.unwrap();

        let ctx2 = ctx_at(now).with_bindings(
            Bindings::resolve(&pipe.params, &[("tag".to_string(), "b".to_string())]).unwrap(),
        );
        let report = engine.eval(&pipe, &mut cache, &ctx2).await.unwrap();
        assert_eq!(report.status(src), Some(&NodeStatus::Cached));
        assert_eq!(
            report.status(pass),
            Some(&NodeStatus::Ok),
            "binding changed"
        );

        let report = engine.eval(&pipe, &mut cache, &ctx2).await.unwrap();
        assert_eq!(
            report.status(pass),
            Some(&NodeStatus::Cached),
            "same binding"
        );
    }

    #[tokio::test]
    async fn undeclared_param_ref_is_rejected_at_validation() {
        let (registry, _) = test_registry();
        let engine = Engine::new(Arc::new(registry));
        let mut pipe = Pipe::new("bad-ref");
        pipe.add_node("test_source", Params::new().with("items", "${nope}"));
        let mut cache = EvalCache::new();
        let err = engine.eval(&pipe, &mut cache, &ctx()).await.unwrap_err();
        assert!(err.to_string().contains("undeclared pipe param"), "{err}");
    }
}
