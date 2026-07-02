//! Filter: keep or drop items by expression rules (`field op value`).

use crate::expr;
use crate::item::PortSpec;
use crate::module::{EvalCtx, ITEMS_IN, ITEMS_OUT, Ins, Module, Outs};
use crate::params::{FieldKind, FieldSpec, ParamSchema, Params};

pub struct Filter;

#[async_trait::async_trait]
impl Module for Filter {
    fn kind(&self) -> &'static str {
        "filter"
    }

    fn inputs(&self) -> &'static [PortSpec] {
        ITEMS_IN
    }

    fn outputs(&self) -> &'static [PortSpec] {
        ITEMS_OUT
    }

    fn param_schema(&self) -> ParamSchema {
        ParamSchema::new(vec![
            FieldSpec::optional("rules", "Rules (field op value)", FieldKind::RuleList),
            FieldSpec::optional("mode", "Mode", FieldKind::Enum(&["permit", "block"]))
                .with_default("permit"),
            FieldSpec::optional("match", "Match", FieldKind::Enum(&["all", "any"]))
                .with_default("all"),
        ])
    }

    async fn eval(&self, ctx: &EvalCtx, ins: Ins, params: &Params) -> anyhow::Result<Outs> {
        let items = ins.items("in")?;
        let rules = compile_rules(params)?;
        // No rules: pass everything through, even in block mode. The
        // vacuous-truth alternative (block+all dropping the whole stream)
        // is a foot-gun while someone is still typing their first rule.
        if rules.is_empty() {
            return Ok(Outs::items(items.to_vec()));
        }
        let permit = match params.get_str("mode").unwrap_or("permit") {
            "permit" => true,
            "block" => false,
            other => anyhow::bail!("mode must be `permit` or `block`, not `{other}`"),
        };
        let match_all = match params.get_str("match").unwrap_or("all") {
            "all" => true,
            "any" => false,
            other => anyhow::bail!("match must be `all` or `any`, not `{other}`"),
        };
        let kept = items
            .iter()
            .filter(|item| {
                let hit = if match_all {
                    rules.iter().all(|r| r.matches(item, ctx.now))
                } else {
                    rules.iter().any(|r| r.matches(item, ctx.now))
                };
                hit == permit
            })
            .cloned()
            .collect();
        Ok(Outs::items(kept))
    }
}

fn compile_rules(params: &Params) -> anyhow::Result<Vec<expr::CompiledRule>> {
    let Some(rules) = params.get("rules") else {
        return Ok(Vec::new());
    };
    let rules = rules
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("param `rules` must be a list of strings"))?;
    rules
        .iter()
        .map(|rule| {
            let src = rule
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("rule `{rule}` is not a string"))?;
            expr::compile(src).map_err(|e| anyhow::anyhow!("rule `{src}`: {e}"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::module::test_support::{items_json, run_op, run_op_at, titles};

    fn posts() -> Vec<crate::item::Item> {
        items_json(json!([
            {"title": "Rust 1.78", "score": 250, "pubDate": "2026-07-01T12:00:00Z"},
            {"title": "Going with Go", "score": 80, "pubDate": "2026-06-01T12:00:00Z"},
            {"title": "Rust for pythonistas", "score": "120"},
        ]))
    }

    fn rules(list: serde_json::Value) -> Params {
        Params::new().with("rules", list)
    }

    #[tokio::test]
    async fn permit_all_combines_rules() {
        let params = rules(json!(["title contains rust", "score > 100"]));
        let out = run_op(&Filter, posts(), params).await.unwrap();
        assert_eq!(titles(&out), ["Rust 1.78", "Rust for pythonistas"]);
    }

    #[tokio::test]
    async fn permit_any_widens() {
        let params = rules(json!(["title contains go", "score > 200"])).with("match", "any");
        let out = run_op(&Filter, posts(), params).await.unwrap();
        assert_eq!(titles(&out), ["Rust 1.78", "Going with Go"]);
    }

    #[tokio::test]
    async fn block_inverts() {
        let params = rules(json!(["title contains rust"])).with("mode", "block");
        let out = run_op(&Filter, posts(), params).await.unwrap();
        assert_eq!(titles(&out), ["Going with Go"]);
    }

    #[tokio::test]
    async fn relative_dates_use_injected_clock() {
        let now = "2026-07-02T12:00:00Z".parse().unwrap();
        let params = rules(json!(["item.pubDate > now - 2d"]));
        let out = run_op_at(&Filter, posts(), params, now).await.unwrap();
        // The dateless item can't match a date rule either.
        assert_eq!(titles(&out), ["Rust 1.78"]);
    }

    #[tokio::test]
    async fn no_rules_and_empty_input_pass_through() {
        let out = run_op(&Filter, posts(), Params::new()).await.unwrap();
        assert_eq!(out.len(), 3);
        let params = rules(json!([])).with("mode", "block");
        let out = run_op(&Filter, posts(), params).await.unwrap();
        assert_eq!(out.len(), 3, "no rules must not block everything");
        let out = run_op(&Filter, vec![], rules(json!(["score > 1"])))
            .await
            .unwrap();
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn bad_rule_is_a_clear_error() {
        let err = run_op(&Filter, posts(), rules(json!(["score >> 1"])))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("unknown operator"), "{err}");
        assert!(err.to_string().contains("score >> 1"), "{err}");
    }
}
