//! Transform: rename/copy/drop top-level item fields.
//!
//! Operations are one-liners, applied in order per item:
//! `rename <field> <new>` — move a field; `copy <field> <new>` — duplicate
//! it; `drop <field>` — remove it. Fields are top-level names (a leading
//! `item.` prefix is accepted and stripped).

use crate::item::{Item, PortSpec};
use crate::module::{EvalCtx, ITEMS_IN, ITEMS_OUT, Ins, Module, Outs};
use crate::params::{FieldKind, FieldSpec, ParamSchema, Params};

pub struct Transform;

#[async_trait::async_trait]
impl Module for Transform {
    fn kind(&self) -> &'static str {
        "transform"
    }

    fn inputs(&self) -> &'static [PortSpec] {
        ITEMS_IN
    }

    fn outputs(&self) -> &'static [PortSpec] {
        ITEMS_OUT
    }

    fn param_schema(&self) -> ParamSchema {
        ParamSchema::new(vec![FieldSpec::optional(
            "ops",
            "Operations (rename/copy/drop)",
            FieldKind::RuleList,
        )])
    }

    async fn eval(&self, _ctx: &EvalCtx, ins: Ins, params: &Params) -> anyhow::Result<Outs> {
        let ops = parse_ops(params)?;
        let items = ins
            .items("in")?
            .iter()
            .map(|item| {
                let mut item = item.clone();
                for op in &ops {
                    op.apply(&mut item);
                }
                item
            })
            .collect();
        Ok(Outs::items(items))
    }
}

enum TransformOp {
    Rename { from: String, to: String },
    Copy { from: String, to: String },
    Drop { field: String },
}

impl TransformOp {
    /// Ops on fields the item doesn't have are no-ops: streams are
    /// heterogeneous and per-item complaints would be noise.
    fn apply(&self, item: &mut Item) {
        match self {
            TransformOp::Rename { from, to } => {
                if let Some(value) = item.0.remove(from) {
                    item.0.insert(to.clone(), value);
                }
            }
            TransformOp::Copy { from, to } => {
                if let Some(value) = item.0.get(from).cloned() {
                    item.0.insert(to.clone(), value);
                }
            }
            TransformOp::Drop { field } => {
                item.0.remove(field);
            }
        }
    }
}

fn parse_ops(params: &Params) -> anyhow::Result<Vec<TransformOp>> {
    let Some(ops) = params.get("ops") else {
        return Ok(Vec::new());
    };
    let ops = ops
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("param `ops` must be a list of strings"))?;
    ops.iter()
        .map(|op| {
            let src = op
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("op `{op}` is not a string"))?;
            parse_op(src).map_err(|e| anyhow::anyhow!("op `{src}`: {e}"))
        })
        .collect()
}

fn parse_op(src: &str) -> Result<TransformOp, String> {
    let field = |s: &str| s.strip_prefix("item.").unwrap_or(s).to_string();
    let tokens: Vec<&str> = src.split_whitespace().collect();
    match tokens.as_slice() {
        ["rename", from, to] => Ok(TransformOp::Rename {
            from: field(from),
            to: field(to),
        }),
        ["copy", from, to] => Ok(TransformOp::Copy {
            from: field(from),
            to: field(to),
        }),
        ["drop", f] => Ok(TransformOp::Drop { field: field(f) }),
        _ => Err(
            "expected `rename <field> <new>`, `copy <field> <new>`, or `drop <field>`".to_string(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::module::test_support::{items_json, run_op};

    fn ops(list: serde_json::Value) -> Params {
        Params::new().with("ops", list)
    }

    #[tokio::test]
    async fn rename_copy_drop_apply_in_order() {
        let items = items_json(json!([{"title": "t", "junk": 1, "score": 5}]));
        let params = ops(json!([
            "rename title headline",
            "copy score points",
            "drop junk",
        ]));
        let out = run_op(&Transform, items, params).await.unwrap();
        assert_eq!(
            serde_json::to_value(&out[0]).unwrap(),
            json!({"headline": "t", "points": 5, "score": 5})
        );
    }

    #[tokio::test]
    async fn item_prefix_is_stripped_and_missing_fields_are_noops() {
        let items = items_json(json!([{"a": 1}, {"b": 2}]));
        let params = ops(json!(["rename item.a item.z", "drop nope"]));
        let out = run_op(&Transform, items, params).await.unwrap();
        assert_eq!(serde_json::to_value(&out[0]).unwrap(), json!({"z": 1}));
        assert_eq!(serde_json::to_value(&out[1]).unwrap(), json!({"b": 2}));
    }

    #[tokio::test]
    async fn no_ops_pass_through_and_bad_op_errors() {
        let items = items_json(json!([{"a": 1}]));
        let out = run_op(&Transform, items.clone(), Params::new())
            .await
            .unwrap();
        assert_eq!(out.len(), 1);
        assert!(
            run_op(&Transform, vec![], Params::new())
                .await
                .unwrap()
                .is_empty()
        );
        let err = run_op(&Transform, items, ops(json!(["explode a"])))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("explode a"), "{err}");
    }
}
