//! Pipe-level parameters and `${name}` interpolation.
//!
//! Pipe params are pipe metadata, not canvas nodes: modules reference them
//! as `${name}` inside any string param field, and the engine expands them
//! at eval time. A node param that is *exactly* `${name}` takes the
//! binding's typed value (so a Number param can fill a numeric field);
//! anywhere else the binding is spliced in as text.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::params::{ParamSchema, Params};

/// Kind of one pipe-level parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PipeParamKind {
    Text,
    Url,
    Number,
}

/// Declaration of one pipe-level parameter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PipeParam {
    pub name: String,
    pub kind: PipeParamKind,
    /// Params without a default must be supplied at run time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
}

impl PipeParam {
    pub fn new(name: impl Into<String>, kind: PipeParamKind) -> Self {
        Self {
            name: name.into(),
            kind,
            default: None,
        }
    }

    pub fn with_default(mut self, default: impl Into<serde_json::Value>) -> Self {
        self.default = Some(default.into());
        self
    }
}

/// Concrete values for a pipe's params in one run: defaults overlaid with
/// runtime overrides (`--param name=value`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Bindings(pub BTreeMap<String, serde_json::Value>);

impl Bindings {
    pub fn empty() -> Self {
        Self::default()
    }

    /// Build bindings for `declared` params from string overrides, coercing
    /// each value to its declared kind. Every declared param must end up
    /// bound (override or default); unknown override names are errors.
    pub fn resolve(
        declared: &[PipeParam],
        overrides: &[(String, String)],
    ) -> Result<Self, Vec<String>> {
        let mut errors = Vec::new();
        let mut bound = BTreeMap::new();
        for (name, raw) in overrides {
            let Some(param) = declared.iter().find(|p| &p.name == name) else {
                errors.push(format!("unknown pipe param `{name}`"));
                continue;
            };
            match coerce(raw, param.kind) {
                Ok(value) => {
                    bound.insert(name.clone(), value);
                }
                Err(e) => errors.push(format!("param `{name}`: {e}")),
            }
        }
        for param in declared {
            // A failed coercion already produced an error; don't also claim
            // the param wasn't supplied.
            let attempted = overrides.iter().any(|(name, _)| name == &param.name);
            if !bound.contains_key(&param.name) && !attempted {
                match &param.default {
                    Some(value) => {
                        bound.insert(param.name.clone(), value.clone());
                    }
                    None => errors.push(format!(
                        "param `{}` requires a value (--param {}=...)",
                        param.name, param.name
                    )),
                }
            }
        }
        if errors.is_empty() {
            Ok(Self(bound))
        } else {
            Err(errors)
        }
    }

    pub fn get(&self, name: &str) -> Option<&serde_json::Value> {
        self.0.get(name)
    }
}

fn coerce(raw: &str, kind: PipeParamKind) -> Result<serde_json::Value, String> {
    match kind {
        PipeParamKind::Text => Ok(raw.into()),
        PipeParamKind::Url => {
            if raw.contains("://") {
                Ok(raw.into())
            } else {
                Err(format!("`{raw}` is not a URL (missing scheme)"))
            }
        }
        PipeParamKind::Number => raw
            .trim()
            .parse::<f64>()
            .map(serde_json::Value::from)
            .map_err(|_| format!("`{raw}` is not a number")),
    }
}

/// Collect every `${name}` referenced in `params`, skipping fields the
/// schema marks as non-interpolated. Used for load-time validation.
pub fn param_refs(params: &Params, schema: &ParamSchema) -> Result<Vec<String>, String> {
    let mut refs = Vec::new();
    for (key, value) in &params.0 {
        if !interpolated(schema, key) {
            continue;
        }
        refs_in_value(value, &mut refs)?;
    }
    refs.sort();
    refs.dedup();
    Ok(refs)
}

/// Expand `${name}` references in `params` against `bindings`. Fields the
/// schema marks as non-interpolated pass through verbatim.
pub fn interpolate_params(
    params: &Params,
    schema: &ParamSchema,
    bindings: &Bindings,
) -> Result<Params, String> {
    let mut out = serde_json::Map::new();
    for (key, value) in &params.0 {
        let value = if interpolated(schema, key) {
            interpolate_value(value, bindings)?
        } else {
            value.clone()
        };
        out.insert(key.clone(), value);
    }
    Ok(Params(out))
}

fn interpolated(schema: &ParamSchema, field: &str) -> bool {
    schema
        .fields
        .iter()
        .find(|f| f.name == field)
        .is_none_or(|f| f.interpolate)
}

fn interpolate_value(
    value: &serde_json::Value,
    bindings: &Bindings,
) -> Result<serde_json::Value, String> {
    match value {
        serde_json::Value::String(s) => interpolate_string(s, bindings),
        serde_json::Value::Array(arr) => arr
            .iter()
            .map(|v| interpolate_value(v, bindings))
            .collect::<Result<Vec<_>, _>>()
            .map(serde_json::Value::Array),
        serde_json::Value::Object(map) => map
            .iter()
            .map(|(k, v)| Ok((k.clone(), interpolate_value(v, bindings)?)))
            .collect::<Result<serde_json::Map<_, _>, String>>()
            .map(serde_json::Value::Object),
        other => Ok(other.clone()),
    }
}

fn interpolate_string(s: &str, bindings: &Bindings) -> Result<serde_json::Value, String> {
    // Exactly `${name}`: take the typed value, so numbers stay numbers.
    if let Some(name) = exact_ref(s) {
        return bindings
            .get(name)
            .cloned()
            .ok_or_else(|| format!("unknown pipe param `${{{name}}}`"));
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after
            .find('}')
            .ok_or_else(|| format!("unclosed `${{` in `{s}`"))?;
        let name = &after[..end];
        let value = bindings
            .get(name)
            .ok_or_else(|| format!("unknown pipe param `${{{name}}}`"))?;
        out.push_str(&render(value));
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out.into())
}

fn refs_in_value(value: &serde_json::Value, refs: &mut Vec<String>) -> Result<(), String> {
    match value {
        serde_json::Value::String(s) => refs_in_string(s, refs),
        serde_json::Value::Array(arr) => arr.iter().try_for_each(|v| refs_in_value(v, refs)),
        serde_json::Value::Object(map) => map.values().try_for_each(|v| refs_in_value(v, refs)),
        _ => Ok(()),
    }
}

fn refs_in_string(s: &str, refs: &mut Vec<String>) -> Result<(), String> {
    let mut rest = s;
    while let Some(start) = rest.find("${") {
        let after = &rest[start + 2..];
        let end = after
            .find('}')
            .ok_or_else(|| format!("unclosed `${{` in `{s}`"))?;
        refs.push(after[..end].to_string());
        rest = &after[end + 1..];
    }
    Ok(())
}

fn exact_ref(s: &str) -> Option<&str> {
    let name = s.strip_prefix("${")?.strip_suffix('}')?;
    // `${a} and ${b}` is not an exact ref even though it has the shape.
    if name.contains('}') || name.contains("${") {
        None
    } else {
        Some(name)
    }
}

fn render(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn declared() -> Vec<PipeParam> {
        vec![
            PipeParam::new("url", PipeParamKind::Url),
            PipeParam::new("count", PipeParamKind::Number).with_default(10),
            PipeParam::new("tag", PipeParamKind::Text).with_default("rust"),
        ]
    }

    fn bindings() -> Bindings {
        Bindings::resolve(
            &declared(),
            &[("url".to_string(), "https://x.dev/feed".to_string())],
        )
        .unwrap()
    }

    #[test]
    fn resolve_overlays_overrides_on_defaults() {
        let b = bindings();
        assert_eq!(b.get("url"), Some(&json!("https://x.dev/feed")));
        assert_eq!(b.get("count"), Some(&json!(10)));
        assert_eq!(b.get("tag"), Some(&json!("rust")));

        let b = Bindings::resolve(
            &declared(),
            &[
                ("url".to_string(), "https://y.dev".to_string()),
                ("count".to_string(), "5".to_string()),
            ],
        )
        .unwrap();
        assert_eq!(b.get("count"), Some(&json!(5.0)));
    }

    #[test]
    fn resolve_errors() {
        let errs = Bindings::resolve(&declared(), &[]).unwrap_err();
        assert_eq!(errs, vec!["param `url` requires a value (--param url=...)"]);

        let errs = Bindings::resolve(
            &declared(),
            &[
                ("url".to_string(), "no-scheme".to_string()),
                ("count".to_string(), "many".to_string()),
                ("bogus".to_string(), "x".to_string()),
            ],
        )
        .unwrap_err();
        assert_eq!(errs.len(), 3, "{errs:?}");
    }

    #[test]
    fn exact_ref_takes_typed_value() {
        let params = Params::new().with("n", "${count}").with("u", "${url}");
        let out = interpolate_params(&params, &ParamSchema::default(), &bindings()).unwrap();
        assert_eq!(out.get("n"), Some(&json!(10)));
        assert_eq!(out.get("u"), Some(&json!("https://x.dev/feed")));
    }

    #[test]
    fn embedded_refs_splice_as_text_including_in_rule_lists() {
        let params = Params::new().with("path", "${url}?limit=${count}").with(
            "rules",
            json!(["title contains ${tag}", "score > ${count}"]),
        );
        let out = interpolate_params(&params, &ParamSchema::default(), &bindings()).unwrap();
        assert_eq!(out.get("path"), Some(&json!("https://x.dev/feed?limit=10")));
        assert_eq!(
            out.get("rules"),
            Some(&json!(["title contains rust", "score > 10"]))
        );
    }

    #[test]
    fn unknown_ref_and_unclosed_brace_are_errors() {
        let params = Params::new().with("u", "${nope}");
        let err = interpolate_params(&params, &ParamSchema::default(), &bindings()).unwrap_err();
        assert!(err.contains("unknown pipe param `${nope}`"), "{err}");

        let params = Params::new().with("u", "x${unclosed");
        let err = interpolate_params(&params, &ParamSchema::default(), &bindings()).unwrap_err();
        assert!(err.contains("unclosed"), "{err}");
    }

    #[test]
    fn schema_can_exempt_fields() {
        use crate::params::{FieldKind, FieldSpec};
        let schema = ParamSchema::new(vec![
            FieldSpec::optional("replacement", "Replacement", FieldKind::Text).no_interpolation(),
        ]);
        let params = Params::new().with("replacement", "${some_capture_group}");
        let out = interpolate_params(&params, &schema, &Bindings::empty()).unwrap();
        assert_eq!(
            out.get("replacement"),
            Some(&json!("${some_capture_group}"))
        );
        assert_eq!(param_refs(&params, &schema).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn param_refs_collects_and_dedupes() {
        let params = Params::new()
            .with("a", "${x} ${y}")
            .with("rules", json!(["f > ${x}"]));
        let refs = param_refs(&params, &ParamSchema::default()).unwrap();
        assert_eq!(refs, vec!["x", "y"]);
    }
}
