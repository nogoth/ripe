//! Reverse: flip the item order.

use crate::item::PortSpec;
use crate::module::{EvalCtx, ITEMS_IN, ITEMS_OUT, Ins, Module, Outs};
use crate::params::{ParamSchema, Params};

pub struct Reverse;

#[async_trait::async_trait]
impl Module for Reverse {
    fn kind(&self) -> &'static str {
        "reverse"
    }

    fn inputs(&self) -> &'static [PortSpec] {
        ITEMS_IN
    }

    fn outputs(&self) -> &'static [PortSpec] {
        ITEMS_OUT
    }

    fn param_schema(&self) -> ParamSchema {
        ParamSchema::default()
    }

    async fn eval(&self, _ctx: &EvalCtx, ins: Ins, _params: &Params) -> anyhow::Result<Outs> {
        let mut items = ins.items("in")?.to_vec();
        items.reverse();
        Ok(Outs::items(items))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::module::test_support::{items_json, run_op, titles};

    #[tokio::test]
    async fn reverses_and_handles_empty() {
        let items = items_json(json!([{"title": "a"}, {"title": "b"}]));
        let out = run_op(&Reverse, items, Params::new()).await.unwrap();
        assert_eq!(titles(&out), ["b", "a"]);
        assert!(
            run_op(&Reverse, vec![], Params::new())
                .await
                .unwrap()
                .is_empty()
        );
    }
}
