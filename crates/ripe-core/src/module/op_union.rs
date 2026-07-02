//! Union: merge any number of item streams, in wire order.

use crate::item::{PortSpec, PortType, PortValue};
use crate::module::{EvalCtx, ITEMS_OUT, Ins, Module, Outs};
use crate::params::{ParamSchema, Params};

pub struct Union;

#[async_trait::async_trait]
impl Module for Union {
    fn kind(&self) -> &'static str {
        "union"
    }

    fn inputs(&self) -> &'static [PortSpec] {
        const IN: &[PortSpec] = &[PortSpec::variadic("in", PortType::Items)];
        IN
    }

    fn outputs(&self) -> &'static [PortSpec] {
        ITEMS_OUT
    }

    fn param_schema(&self) -> ParamSchema {
        ParamSchema::default()
    }

    async fn eval(&self, _ctx: &EvalCtx, ins: Ins, _params: &Params) -> anyhow::Result<Outs> {
        let mut merged = Vec::new();
        for value in ins.all("in") {
            match value {
                PortValue::Items(items) => merged.extend(items.iter().cloned()),
                other => anyhow::bail!("union expected items, got {:?}", other.port_type()),
            }
        }
        Ok(Outs::items(merged))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::module::test_support::{ctx_at, items_json, titles};

    #[tokio::test]
    async fn merges_in_wire_order() {
        let mut ins = Ins::default();
        ins.push(
            "in",
            PortValue::Items(items_json(json!([{"title": "a"}, {"title": "b"}]))),
        );
        ins.push("in", PortValue::Items(items_json(json!([{"title": "c"}]))));
        let outs = Union
            .eval(&ctx_at(chrono::Utc::now()), ins, &Params::new())
            .await
            .unwrap();
        let Some(PortValue::Items(items)) = outs.get("out") else {
            panic!()
        };
        assert_eq!(titles(items), ["a", "b", "c"]);
    }

    #[tokio::test]
    async fn zero_wires_yield_empty_stream() {
        let outs = Union
            .eval(&ctx_at(chrono::Utc::now()), Ins::default(), &Params::new())
            .await
            .unwrap();
        assert_eq!(outs.item_count(), Some(0));
    }
}
