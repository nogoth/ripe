//! Output: marks the pipe's terminal stream and carries the emit settings
//! (format + destination).
//!
//! Eval is a pure passthrough — previews re-run it on every edit, so the
//! actual writing is done by the runner (M7) and the TUI's run command,
//! which read this node's params and the cached stream.

use crate::item::PortSpec;
use crate::module::{EvalCtx, ITEMS_IN, ITEMS_OUT, Ins, Module, Outs};
use crate::params::{FieldKind, FieldSpec, ParamSchema, Params};

pub struct Output;

#[async_trait::async_trait]
impl Module for Output {
    fn kind(&self) -> &'static str {
        "output"
    }

    fn inputs(&self) -> &'static [PortSpec] {
        ITEMS_IN
    }

    // Terminal by convention (nothing wires from it in practice), but it
    // echoes its input so the preview/runner can read the final stream
    // straight from this node's cache entry.
    fn outputs(&self) -> &'static [PortSpec] {
        ITEMS_OUT
    }

    fn param_schema(&self) -> ParamSchema {
        ParamSchema::new(vec![
            FieldSpec::optional(
                "format",
                "Format",
                FieldKind::Enum(&["rss", "atom", "json", "csv"]),
            )
            .with_default("rss"),
            FieldSpec::optional(
                "destination",
                "Destination (empty = stdout)",
                FieldKind::Text,
            )
            .with_default(""),
        ])
    }

    async fn eval(&self, _ctx: &EvalCtx, ins: Ins, _params: &Params) -> anyhow::Result<Outs> {
        Ok(Outs::items(ins.items("in")?.to_vec()))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::module::test_support::{items_json, run_op, titles};

    #[tokio::test]
    async fn passes_items_through_untouched() {
        let items = items_json(json!([{"title": "a"}, {"title": "b"}]));
        let out = run_op(&Output, items, Params::new()).await.unwrap();
        assert_eq!(titles(&out), ["a", "b"]);
        assert!(
            run_op(&Output, vec![], Params::new())
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn registered_as_builtin() {
        assert!(
            crate::module::Registry::with_builtins()
                .get("output")
                .is_some()
        );
    }
}
