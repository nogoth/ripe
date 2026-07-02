//! Limit (first N) and Tail (last N).

use crate::item::PortSpec;
use crate::module::{EvalCtx, ITEMS_IN, ITEMS_OUT, Ins, Module, Outs};
use crate::params::{FieldKind, FieldSpec, ParamSchema, Params};

fn n_param(params: &Params) -> anyhow::Result<usize> {
    params
        .get_usize("n")
        .ok_or_else(|| anyhow::anyhow!("param `n` must be a non-negative integer"))
}

fn n_schema() -> ParamSchema {
    ParamSchema::new(vec![FieldSpec::required(
        "n",
        "Number of items",
        FieldKind::Number,
    )])
}

pub struct Limit;

#[async_trait::async_trait]
impl Module for Limit {
    fn kind(&self) -> &'static str {
        "limit"
    }

    fn inputs(&self) -> &'static [PortSpec] {
        ITEMS_IN
    }

    fn outputs(&self) -> &'static [PortSpec] {
        ITEMS_OUT
    }

    fn param_schema(&self) -> ParamSchema {
        n_schema()
    }

    async fn eval(&self, _ctx: &EvalCtx, ins: Ins, params: &Params) -> anyhow::Result<Outs> {
        let n = n_param(params)?;
        let mut items = ins.items("in")?.to_vec();
        items.truncate(n);
        Ok(Outs::items(items))
    }
}

pub struct Tail;

#[async_trait::async_trait]
impl Module for Tail {
    fn kind(&self) -> &'static str {
        "tail"
    }

    fn inputs(&self) -> &'static [PortSpec] {
        ITEMS_IN
    }

    fn outputs(&self) -> &'static [PortSpec] {
        ITEMS_OUT
    }

    fn param_schema(&self) -> ParamSchema {
        n_schema()
    }

    async fn eval(&self, _ctx: &EvalCtx, ins: Ins, params: &Params) -> anyhow::Result<Outs> {
        let n = n_param(params)?;
        let items = ins.items("in")?;
        let skip = items.len().saturating_sub(n);
        Ok(Outs::items(items[skip..].to_vec()))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::module::test_support::{items_json, run_op, titles};

    fn three() -> Vec<crate::item::Item> {
        items_json(json!([{"title": "a"}, {"title": "b"}, {"title": "c"}]))
    }

    #[tokio::test]
    async fn limit_takes_first_n() {
        let out = run_op(&Limit, three(), Params::new().with("n", 2))
            .await
            .unwrap();
        assert_eq!(titles(&out), ["a", "b"]);
    }

    #[tokio::test]
    async fn tail_takes_last_n() {
        let out = run_op(&Tail, three(), Params::new().with("n", 2))
            .await
            .unwrap();
        assert_eq!(titles(&out), ["b", "c"]);
    }

    #[tokio::test]
    async fn n_larger_than_input_and_zero_and_empty() {
        assert_eq!(
            run_op(&Limit, three(), Params::new().with("n", 9))
                .await
                .unwrap()
                .len(),
            3
        );
        assert_eq!(
            run_op(&Tail, three(), Params::new().with("n", 9))
                .await
                .unwrap()
                .len(),
            3
        );
        assert!(
            run_op(&Limit, three(), Params::new().with("n", 0))
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            run_op(&Tail, vec![], Params::new().with("n", 2))
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn bad_n_is_an_error() {
        for params in [
            Params::new(),
            Params::new().with("n", -1),
            Params::new().with("n", 1.5),
        ] {
            let err = run_op(&Limit, three(), params).await.unwrap_err();
            assert!(err.to_string().contains("`n`"), "{err}");
        }
    }
}
