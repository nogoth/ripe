//! Sort: order items by a field — numeric, lexical, or date-aware.

use std::cmp::Ordering;

use crate::expr::{parse_date, value_f64, value_text};
use crate::item::{Item, PortSpec};
use crate::module::{EvalCtx, ITEMS_IN, ITEMS_OUT, Ins, Module, Outs};
use crate::params::{FieldKind, FieldSpec, ParamSchema, Params};

pub struct Sort;

#[async_trait::async_trait]
impl Module for Sort {
    fn kind(&self) -> &'static str {
        "sort"
    }

    fn inputs(&self) -> &'static [PortSpec] {
        ITEMS_IN
    }

    fn outputs(&self) -> &'static [PortSpec] {
        ITEMS_OUT
    }

    fn param_schema(&self) -> ParamSchema {
        ParamSchema::new(vec![
            FieldSpec::required("by", "Sort by field", FieldKind::FieldName),
            FieldSpec::optional("order", "Order", FieldKind::Enum(&["asc", "desc"]))
                .with_default("asc"),
            FieldSpec::optional(
                "compare",
                "Compare as",
                FieldKind::Enum(&["auto", "numeric", "lexical", "date"]),
            )
            .with_default("auto"),
        ])
    }

    async fn eval(&self, _ctx: &EvalCtx, ins: Ins, params: &Params) -> anyhow::Result<Outs> {
        let items = ins.items("in")?;
        let by = params
            .get_str("by")
            .ok_or_else(|| anyhow::anyhow!("param `by` is required"))?;
        let descending = match params.get_str("order").unwrap_or("asc") {
            "asc" => false,
            "desc" => true,
            other => anyhow::bail!("order must be `asc` or `desc`, not `{other}`"),
        };
        let mode = match params.get_str("compare").unwrap_or("auto") {
            "auto" => detect_mode(items, by),
            m @ ("numeric" | "lexical" | "date") => m,
            other => anyhow::bail!(
                "compare must be `auto`, `numeric`, `lexical`, or `date`, not `{other}`"
            ),
        };

        let mut keyed: Vec<(SortKey, Item)> = items
            .iter()
            .map(|item| (SortKey::extract(item, by, mode), item.clone()))
            .collect();
        // Stable sort; missing-field items always sink to the end,
        // regardless of direction.
        keyed.sort_by(|(a, _), (b, _)| match (a, b) {
            (SortKey::Missing, SortKey::Missing) => Ordering::Equal,
            (SortKey::Missing, _) => Ordering::Greater,
            (_, SortKey::Missing) => Ordering::Less,
            (a, b) => {
                let natural = a.cmp_same_kind(b);
                if descending {
                    natural.reverse()
                } else {
                    natural
                }
            }
        });
        Ok(Outs::items(
            keyed.into_iter().map(|(_, item)| item).collect(),
        ))
    }
}

/// Auto mode: numeric if every present value is numeric, else date-aware if
/// every present value parses as a date, else lexical. Missing fields don't
/// vote.
fn detect_mode<'a>(items: impl IntoIterator<Item = &'a Item>, field: &str) -> &'static str {
    let present: Vec<&serde_json::Value> = items
        .into_iter()
        .filter_map(|item| item.get(field))
        .collect();
    if present.is_empty() {
        "lexical"
    } else if present.iter().all(|v| value_f64(v).is_some()) {
        "numeric"
    } else if present.iter().all(|v| parse_date(&value_text(v)).is_some()) {
        "date"
    } else {
        "lexical"
    }
}

#[derive(Debug)]
enum SortKey {
    Num(f64),
    Date(i64),
    Str(String),
    Missing,
}

impl SortKey {
    fn extract(item: &Item, field: &str, mode: &str) -> Self {
        let Some(value) = item.get(field) else {
            return SortKey::Missing;
        };
        match mode {
            "numeric" => value_f64(value).map_or(SortKey::Missing, SortKey::Num),
            "date" => parse_date(&value_text(value))
                .map_or(SortKey::Missing, |d| SortKey::Date(d.timestamp_millis())),
            _ => SortKey::Str(value_text(value)),
        }
    }

    fn cmp_same_kind(&self, other: &Self) -> Ordering {
        match (self, other) {
            (SortKey::Num(a), SortKey::Num(b)) => a.partial_cmp(b).unwrap_or(Ordering::Equal),
            (SortKey::Date(a), SortKey::Date(b)) => a.cmp(b),
            (SortKey::Str(a), SortKey::Str(b)) => a.cmp(b),
            _ => Ordering::Equal, // mixed kinds cannot happen: one mode per run
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::module::test_support::{items_json, run_op, titles};

    fn by(field: &str) -> Params {
        Params::new().with("by", field)
    }

    #[tokio::test]
    async fn numeric_auto_detects_and_coerces_strings() {
        let items = items_json(json!([
            {"title": "b", "score": 100},
            {"title": "a", "score": "20"},
            {"title": "c", "score": 3},
        ]));
        let out = run_op(&Sort, items, by("score")).await.unwrap();
        assert_eq!(titles(&out), ["c", "a", "b"]);
    }

    #[tokio::test]
    async fn desc_and_lexical_fallback() {
        let items = items_json(json!([
            {"title": "banana"}, {"title": "Cherry"}, {"title": "apple"},
        ]));
        let out = run_op(&Sort, items.clone(), by("title")).await.unwrap();
        assert_eq!(titles(&out), ["Cherry", "apple", "banana"]); // ASCII order
        let out = run_op(&Sort, items, by("title").with("order", "desc"))
            .await
            .unwrap();
        assert_eq!(titles(&out), ["banana", "apple", "Cherry"]);
    }

    #[tokio::test]
    async fn date_aware_sorts_mixed_formats() {
        let items = items_json(json!([
            {"title": "mid", "pubDate": "Mon, 15 Jun 2026 10:00:00 GMT"},
            {"title": "new", "pubDate": "2026-07-01T12:00:00Z"},
            {"title": "old", "pubDate": "2026-01-05"},
        ]));
        let out = run_op(&Sort, items, by("pubDate").with("order", "desc"))
            .await
            .unwrap();
        assert_eq!(titles(&out), ["new", "mid", "old"]);
    }

    #[tokio::test]
    async fn missing_fields_sink_to_the_end_both_directions() {
        let items = items_json(json!([
            {"title": "no-score"},
            {"title": "hi", "score": 9},
            {"title": "lo", "score": 1},
        ]));
        let out = run_op(&Sort, items.clone(), by("score")).await.unwrap();
        assert_eq!(titles(&out), ["lo", "hi", "no-score"]);
        let out = run_op(&Sort, items, by("score").with("order", "desc"))
            .await
            .unwrap();
        assert_eq!(titles(&out), ["hi", "lo", "no-score"]);
    }

    #[tokio::test]
    async fn empty_input_and_missing_param() {
        assert!(run_op(&Sort, vec![], by("x")).await.unwrap().is_empty());
        let err = run_op(&Sort, vec![], Params::new()).await.unwrap_err();
        assert!(err.to_string().contains("`by`"), "{err}");
    }
}
