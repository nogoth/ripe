//! Unique: drop items whose field value was already seen (first one wins).

use std::collections::HashSet;

use crate::item::PortSpec;
use crate::module::{EvalCtx, ITEMS_IN, ITEMS_OUT, Ins, Module, Outs};
use crate::params::{FieldKind, FieldSpec, ParamSchema, Params};

pub struct Unique;

#[async_trait::async_trait]
impl Module for Unique {
    fn kind(&self) -> &'static str {
        "unique"
    }

    fn inputs(&self) -> &'static [PortSpec] {
        ITEMS_IN
    }

    fn outputs(&self) -> &'static [PortSpec] {
        ITEMS_OUT
    }

    fn param_schema(&self) -> ParamSchema {
        ParamSchema::new(vec![FieldSpec::required(
            "by",
            "Dedupe by field",
            FieldKind::FieldName,
        )])
    }

    async fn eval(&self, _ctx: &EvalCtx, ins: Ins, params: &Params) -> anyhow::Result<Outs> {
        let by = params
            .get_str("by")
            .ok_or_else(|| anyhow::anyhow!("param `by` is required"))?;
        let mut seen = HashSet::new();
        let kept = ins
            .items("in")?
            .iter()
            .filter(|item| match item.get_str(by) {
                // Items missing the field are all kept: there is no value
                // to say they duplicate each other.
                None => true,
                Some(value) => seen.insert(value),
            })
            .cloned()
            .collect();
        Ok(Outs::items(kept))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::module::test_support::{items_json, run_op, titles};

    #[tokio::test]
    async fn first_occurrence_wins_and_missing_fields_are_kept() {
        let items = items_json(json!([
            {"title": "a", "link": "https://x.dev/1"},
            {"title": "b"},
            {"title": "dup", "link": "https://x.dev/1"},
            {"title": "c", "link": "https://x.dev/2"},
            {"title": "d"},
        ]));
        let out = run_op(&Unique, items, Params::new().with("by", "link"))
            .await
            .unwrap();
        assert_eq!(titles(&out), ["a", "b", "c", "d"]);
    }

    #[tokio::test]
    async fn empty_input_and_missing_param() {
        assert!(
            run_op(&Unique, vec![], Params::new().with("by", "x"))
                .await
                .unwrap()
                .is_empty()
        );
        let err = run_op(&Unique, vec![], Params::new()).await.unwrap_err();
        assert!(err.to_string().contains("`by`"), "{err}");
    }
}
