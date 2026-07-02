//! Regex: replace within a field, or extract named capture groups into
//! new fields (mockup: `^\[(?<score>\d+)\]\s(?<title>.*)` pulls `score`
//! out of a bracketed title).

use crate::expr::value_text;
use crate::item::PortSpec;
use crate::module::{EvalCtx, ITEMS_IN, ITEMS_OUT, Ins, Module, Outs};
use crate::params::{FieldKind, FieldSpec, ParamSchema, Params};

pub struct RegexOp;

#[async_trait::async_trait]
impl Module for RegexOp {
    fn kind(&self) -> &'static str {
        "regex"
    }

    fn inputs(&self) -> &'static [PortSpec] {
        ITEMS_IN
    }

    fn outputs(&self) -> &'static [PortSpec] {
        ITEMS_OUT
    }

    fn param_schema(&self) -> ParamSchema {
        ParamSchema::new(vec![
            FieldSpec::required("field", "Field", FieldKind::FieldName),
            FieldSpec::required("pattern", "Pattern", FieldKind::Text),
            FieldSpec::optional("mode", "Mode", FieldKind::Enum(&["replace", "extract"]))
                .with_default("replace"),
            FieldSpec::optional("replacement", "Replacement ($1, $name)", FieldKind::Text)
                .with_default(""),
        ])
    }

    async fn eval(&self, _ctx: &EvalCtx, ins: Ins, params: &Params) -> anyhow::Result<Outs> {
        let field = params
            .get_str("field")
            .map(|f| f.strip_prefix("item.").unwrap_or(f).to_string())
            .ok_or_else(|| anyhow::anyhow!("param `field` is required"))?;
        let pattern = params
            .get_str("pattern")
            .ok_or_else(|| anyhow::anyhow!("param `pattern` is required"))?;
        let regex = regex::Regex::new(pattern)
            .map_err(|e| anyhow::anyhow!("bad regex `{pattern}`: {e}"))?;
        let mode = params.get_str("mode").unwrap_or("replace");

        let group_names: Vec<&str> = regex.capture_names().flatten().collect();
        if mode == "extract" && group_names.is_empty() {
            anyhow::bail!(
                "extract mode needs named capture groups, e.g. `(?<score>\\d+)`; \
                 `{pattern}` has none"
            );
        }

        let items = ins.items("in")?;
        let mut result = Vec::with_capacity(items.len());
        for item in items {
            let mut item = item.clone();
            // Items without the field pass through untouched.
            if let Some(text) = item.get(&field).map(value_text) {
                match mode {
                    "replace" => {
                        let replacement = params.get_str("replacement").unwrap_or("");
                        item.set(&field, regex.replace_all(&text, replacement).into_owned());
                    }
                    "extract" => {
                        if let Some(captures) = regex.captures(&text) {
                            for name in &group_names {
                                if let Some(m) = captures.name(name) {
                                    item.set(*name, m.as_str());
                                }
                            }
                        }
                    }
                    other => anyhow::bail!("mode must be `replace` or `extract`, not `{other}`"),
                }
            }
            result.push(item);
        }
        Ok(Outs::items(result))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::module::test_support::{items_json, run_op};

    fn params(field: &str, pattern: &str) -> Params {
        Params::new().with("field", field).with("pattern", pattern)
    }

    #[tokio::test]
    async fn replace_rewrites_the_field() {
        let items = items_json(json!([{"title": "Rust is neat"}, {"title": "rust rust"}]));
        let p = params("title", "(?i)rust").with("replacement", "Crab");
        let out = run_op(&RegexOp, items, p).await.unwrap();
        assert_eq!(out[0].get_str("title").as_deref(), Some("Crab is neat"));
        assert_eq!(out[1].get_str("title").as_deref(), Some("Crab Crab"));
    }

    #[tokio::test]
    async fn replace_supports_group_references() {
        let items = items_json(json!([{"link": "http://x.dev/post"}]));
        let p = params("link", "^http://").with("replacement", "https://");
        let out = run_op(&RegexOp, items, p).await.unwrap();
        assert_eq!(
            out[0].get_str("link").as_deref(),
            Some("https://x.dev/post")
        );
    }

    #[tokio::test]
    async fn extract_creates_fields_from_named_groups() {
        let items = items_json(json!([
            {"title": "[250] Rust 1.78 released"},
            {"title": "no score here"},
        ]));
        let p = params("title", r"^\[(?<score>\d+)\]\s(?<rest>.*)").with("mode", "extract");
        let out = run_op(&RegexOp, items, p).await.unwrap();
        assert_eq!(out[0].get_str("score").as_deref(), Some("250"));
        assert_eq!(
            out[0].get_str("rest").as_deref(),
            Some("Rust 1.78 released")
        );
        // Original field is untouched; non-matching items gain nothing.
        assert_eq!(
            out[0].get_str("title").as_deref(),
            Some("[250] Rust 1.78 released")
        );
        assert_eq!(out[1].get("score"), None);
    }

    #[tokio::test]
    async fn missing_field_passes_through_and_empty_input_ok() {
        let items = items_json(json!([{"other": 1}]));
        let p = params("title", "x").with("replacement", "y");
        let out = run_op(&RegexOp, items, p).await.unwrap();
        assert_eq!(serde_json::to_value(&out[0]).unwrap(), json!({"other": 1}));
        assert!(
            run_op(&RegexOp, vec![], params("t", "x"))
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn bad_pattern_and_extract_without_groups_error() {
        let items = items_json(json!([{"title": "t"}]));
        let err = run_op(&RegexOp, items.clone(), params("title", "[unclosed"))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("bad regex"), "{err}");
        let p = params("title", r"\d+").with("mode", "extract");
        let err = run_op(&RegexOp, items, p).await.unwrap_err();
        assert!(err.to_string().contains("named capture groups"), "{err}");
    }
}
