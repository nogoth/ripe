//! Filter rule expressions: `field op value`.
//!
//! The grammar is deliberately frozen at comparisons plus relative dates
//! (see PLAN.md, Risks): ops are `==` `!=` `>` `<` `>=` `<=` `contains`
//! `matches`; values are numbers, strings (bare or quoted), or
//! `now ± <n><unit>` with units m/h/d/w. Rules are stored as source text in
//! the pipe file and parsed at load/eval time.

use chrono::{DateTime, Utc};

use crate::item::Item;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Eq,
    Ne,
    Gt,
    Lt,
    Ge,
    Le,
    Contains,
    Matches,
}

impl Op {
    fn parse(token: &str) -> Result<Self, String> {
        match token {
            "==" => Ok(Op::Eq),
            "!=" => Ok(Op::Ne),
            ">" => Ok(Op::Gt),
            "<" => Ok(Op::Lt),
            ">=" => Ok(Op::Ge),
            "<=" => Ok(Op::Le),
            "contains" => Ok(Op::Contains),
            "matches" => Ok(Op::Matches),
            other => Err(format!(
                "unknown operator `{other}` (expected ==, !=, >, <, >=, <=, contains, matches)"
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum RuleValue {
    Number(f64),
    Text(String),
    /// `now ± duration`, resolved against the engine's clock at eval time so
    /// tests (and cached runs) stay deterministic.
    RelativeDate {
        offset_secs: i64,
    },
}

impl RuleValue {
    fn as_text(&self) -> String {
        match self {
            RuleValue::Text(s) => s.clone(),
            RuleValue::Number(n) => n.to_string(),
            RuleValue::RelativeDate { .. } => String::new(),
        }
    }
}

/// A parsed `field op value` rule.
#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
    pub field: String,
    pub op: Op,
    pub value: RuleValue,
}

/// Parse one rule line, e.g. `item.pubDate > now - 2d` or `score > 100`.
pub fn parse_rule(src: &str) -> Result<Rule, String> {
    let src = src.trim();
    if src.is_empty() {
        return Err("empty rule".to_string());
    }
    let (field, rest) = src
        .split_once(char::is_whitespace)
        .ok_or_else(|| format!("rule `{src}` is missing an operator"))?;
    let rest = rest.trim_start();
    let (op_token, value_src) = match rest.split_once(char::is_whitespace) {
        Some((op, value)) => (op, value.trim()),
        None => (rest, ""),
    };
    let op = Op::parse(op_token)?;
    if value_src.is_empty() {
        return Err(format!("rule `{src}` is missing a value"));
    }
    let field = field.strip_prefix("item.").unwrap_or(field);
    Ok(Rule {
        field: field.to_string(),
        op,
        value: parse_value(value_src)?,
    })
}

fn parse_value(src: &str) -> Result<RuleValue, String> {
    for quote in ['"', '\''] {
        if let Some(inner) = src.strip_prefix(quote) {
            return inner
                .strip_suffix(quote)
                .map(|s| RuleValue::Text(s.to_string()))
                .ok_or_else(|| format!("unclosed {quote} quote in `{src}`"));
        }
    }
    if src == "now" {
        return Ok(RuleValue::RelativeDate { offset_secs: 0 });
    }
    if let Some(rest) = src.strip_prefix("now") {
        return parse_now_offset(rest.trim_start())
            .map(|offset_secs| RuleValue::RelativeDate { offset_secs })
            .map_err(|e| format!("bad relative date `{src}`: {e}"));
    }
    if let Ok(n) = src.parse::<f64>() {
        return Ok(RuleValue::Number(n));
    }
    Ok(RuleValue::Text(src.to_string()))
}

/// Parse `± <n><unit>` where unit is m/h/d/w.
fn parse_now_offset(src: &str) -> Result<i64, String> {
    let (sign, rest) = if let Some(r) = src.strip_prefix('+') {
        (1.0, r)
    } else if let Some(r) = src.strip_prefix('-') {
        (-1.0, r)
    } else {
        return Err("expected `+` or `-` after `now`".to_string());
    };
    let rest = rest.trim();
    if rest.is_empty() || !rest.is_ascii() {
        return Err("expected a duration like `2d`".to_string());
    }
    let (number, unit) = rest.split_at(rest.len() - 1);
    let number: f64 = number
        .trim()
        .parse()
        .map_err(|_| format!("`{number}` is not a number"))?;
    let unit_secs = match unit {
        "m" => 60.0,
        "h" => 3600.0,
        "d" => 86_400.0,
        "w" => 604_800.0,
        other => return Err(format!("unknown unit `{other}` (expected m, h, d, or w)")),
    };
    Ok((sign * number * unit_secs) as i64)
}

/// Parse a date the way feeds write them: RFC 3339 (our normalized form),
/// RFC 2822 (raw RSS), or common date-only forms.
pub fn parse_date(s: &str) -> Option<DateTime<Utc>> {
    let s = s.trim();
    if let Ok(d) = DateTime::parse_from_rfc3339(s) {
        return Some(d.with_timezone(&Utc));
    }
    if let Ok(d) = DateTime::parse_from_rfc2822(s) {
        return Some(d.with_timezone(&Utc));
    }
    if let Ok(d) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S") {
        return Some(d.and_utc());
    }
    if let Ok(d) = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Some(d.and_hms_opt(0, 0, 0)?.and_utc());
    }
    None
}

/// A rule ready to evaluate (regex pre-compiled for `matches`).
#[derive(Debug)]
pub struct CompiledRule {
    rule: Rule,
    regex: Option<regex::Regex>,
}

/// Parse and compile one rule line.
pub fn compile(src: &str) -> Result<CompiledRule, String> {
    CompiledRule::new(parse_rule(src)?)
}

impl CompiledRule {
    pub fn new(rule: Rule) -> Result<Self, String> {
        let regex = if rule.op == Op::Matches {
            let RuleValue::Text(pattern) = &rule.value else {
                return Err("`matches` needs a regex pattern, not a number or date".to_string());
            };
            Some(regex::Regex::new(pattern).map_err(|e| format!("bad regex: {e}"))?)
        } else {
            None
        };
        Ok(Self { rule, regex })
    }

    /// Does `item` satisfy this rule? A missing field never matches — not
    /// even `!=` — so filters behave predictably on heterogeneous items.
    pub fn matches(&self, item: &Item, now: DateTime<Utc>) -> bool {
        let Some(value) = item.get(&self.rule.field) else {
            return false;
        };
        let text = value_text(value);
        match self.rule.op {
            Op::Contains => {
                // Case-insensitive: filtering feed titles is the common case.
                text.to_lowercase()
                    .contains(&self.rule.value.as_text().to_lowercase())
            }
            Op::Matches => self.regex.as_ref().is_some_and(|r| r.is_match(&text)),
            op => {
                let ordering = match &self.rule.value {
                    RuleValue::Number(n) => value_f64(value).and_then(|f| f.partial_cmp(n)),
                    RuleValue::RelativeDate { offset_secs } => parse_date(&text)
                        .map(|d| d.cmp(&(now + chrono::Duration::seconds(*offset_secs)))),
                    RuleValue::Text(s) => Some(text.as_str().cmp(s.as_str())),
                };
                ordering.is_some_and(|o| op_holds(op, o))
            }
        }
    }
}

fn op_holds(op: Op, ordering: std::cmp::Ordering) -> bool {
    use std::cmp::Ordering::*;
    match op {
        Op::Eq => ordering == Equal,
        Op::Ne => ordering != Equal,
        Op::Gt => ordering == Greater,
        Op::Lt => ordering == Less,
        Op::Ge => ordering != Less,
        Op::Le => ordering != Greater,
        Op::Contains | Op::Matches => unreachable!("handled above"),
    }
}

/// String view of a field value (strings as-is, scalars rendered).
pub fn value_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Numeric view of a field value: numbers directly, numeric strings parsed.
pub fn value_f64(value: &serde_json::Value) -> Option<f64> {
    match value {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn item(v: serde_json::Value) -> Item {
        Item(v.as_object().unwrap().clone())
    }

    fn now() -> DateTime<Utc> {
        "2026-07-02T12:00:00Z".parse().unwrap()
    }

    fn check(rule: &str, item_json: serde_json::Value) -> bool {
        compile(rule).unwrap().matches(&item(item_json), now())
    }

    #[test]
    fn parse_forms() {
        assert_eq!(
            parse_rule("score > 100").unwrap(),
            Rule {
                field: "score".into(),
                op: Op::Gt,
                value: RuleValue::Number(100.0)
            }
        );
        assert_eq!(
            parse_rule("item.pubDate > now - 2d").unwrap(),
            Rule {
                field: "pubDate".into(),
                op: Op::Gt,
                value: RuleValue::RelativeDate {
                    offset_secs: -2 * 86_400
                }
            }
        );
        assert_eq!(
            parse_rule("title contains rust lang").unwrap().value,
            RuleValue::Text("rust lang".into())
        );
        assert_eq!(
            parse_rule("author == \"Ada Lovelace\"").unwrap().value,
            RuleValue::Text("Ada Lovelace".into())
        );
        assert_eq!(
            parse_rule("t <= now + 1.5h").unwrap().value,
            RuleValue::RelativeDate { offset_secs: 5400 }
        );
        assert_eq!(
            parse_rule("t == now").unwrap().value,
            RuleValue::RelativeDate { offset_secs: 0 }
        );
    }

    #[test]
    fn parse_errors_are_clear() {
        for (src, needle) in [
            ("", "empty rule"),
            ("title", "missing an operator"),
            ("title ~= x", "unknown operator"),
            ("title ==", "missing a value"),
            ("title == \"unclosed", "unclosed"),
            ("t > now * 2d", "expected `+` or `-`"),
            ("t > now - 2y", "unknown unit"),
            ("t > now - d", "not a number"),
        ] {
            let err = parse_rule(src).unwrap_err();
            assert!(err.contains(needle), "`{src}`: {err}");
        }
        // Compile-time (not parse-time) errors.
        assert!(
            compile("t matches [unclosed")
                .unwrap_err()
                .contains("bad regex")
        );
        assert!(
            compile("t matches 42")
                .unwrap_err()
                .contains("regex pattern")
        );
    }

    #[test]
    fn numeric_comparisons_coerce_strings() {
        assert!(check("score > 100", json!({"score": 150})));
        assert!(!check("score > 100", json!({"score": 50})));
        assert!(check("score > 100", json!({"score": "150"})));
        assert!(check("score == 100", json!({"score": "100"})));
        assert!(!check("score > 100", json!({"score": "not a number"})));
    }

    #[test]
    fn relative_dates_compare_rfc3339_and_rfc2822() {
        let fresh = json!({"pubDate": "2026-07-01T12:00:00Z"});
        let stale = json!({"pubDate": "Mon, 01 Jun 2026 10:00:00 GMT"});
        assert!(check("pubDate > now - 2d", fresh.clone()));
        assert!(!check("pubDate > now - 2d", stale.clone()));
        assert!(check("pubDate < now", fresh));
        assert!(check("item.pubDate <= now - 4w", stale));
    }

    #[test]
    fn text_ops() {
        assert!(check(
            "title contains RUST",
            json!({"title": "Why Rust won"})
        ));
        assert!(!check(
            "title contains go",
            json!({"title": "Why Rust won"})
        ));
        assert!(check("title == exact", json!({"title": "exact"})));
        assert!(check("title != other", json!({"title": "exact"})));
        assert!(check(
            "link matches ^https://",
            json!({"link": "https://x.dev"})
        ));
        assert!(!check(
            "link matches ^https://",
            json!({"link": "http://x.dev"})
        ));
    }

    #[test]
    fn missing_field_never_matches() {
        for rule in ["x == 1", "x != 1", "x contains a", "x matches .", "x < now"] {
            assert!(!check(rule, json!({"y": 1})), "{rule}");
        }
    }

    #[test]
    fn date_parser_forms() {
        for s in [
            "2026-07-01T12:00:00Z",
            "2026-07-01T12:00:00+02:00",
            "Mon, 01 Jun 2026 10:00:00 GMT",
            "2026-07-01 12:00:00",
            "2026-07-01",
        ] {
            assert!(parse_date(s).is_some(), "{s}");
        }
        assert!(parse_date("not a date").is_none());
    }
}
