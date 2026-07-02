//! Module parameters and their schemas.
//!
//! A `ParamSchema` is the single source of truth for a module's settings:
//! the TUI renders its param overlay from it, persistence validates against
//! it, and modules read values through typed getters.

use serde::{Deserialize, Serialize};

/// Parameter values for one node, stored as a JSON object so persistence
/// and hashing are uniform with `Item`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Params(pub serde_json::Map<String, serde_json::Value>);

impl Params {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&mut self, key: impl Into<String>, value: impl Into<serde_json::Value>) {
        self.0.insert(key.into(), value.into());
    }

    pub fn with(mut self, key: impl Into<String>, value: impl Into<serde_json::Value>) -> Self {
        self.set(key, value);
        self
    }

    pub fn get(&self, key: &str) -> Option<&serde_json::Value> {
        self.0.get(key)
    }

    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.get(key)?.as_str()
    }

    pub fn get_f64(&self, key: &str) -> Option<f64> {
        self.get(key)?.as_f64()
    }

    pub fn get_usize(&self, key: &str) -> Option<usize> {
        let n = self.get(key)?.as_f64()?;
        if n >= 0.0 && n.fract() == 0.0 {
            Some(n as usize)
        } else {
            None
        }
    }

    pub fn get_bool(&self, key: &str) -> Option<bool> {
        self.get(key)?.as_bool()
    }

    /// The value for `key`, or the schema default, as a string. Convenience
    /// for the common "text param with a default" case.
    pub fn str_or_default(&self, key: &str, schema: &ParamSchema) -> Option<String> {
        if let Some(s) = self.get_str(key) {
            return Some(s.to_string());
        }
        schema
            .fields
            .iter()
            .find(|f| f.name == key)
            .and_then(|f| f.default.as_ref())
            .and_then(|v| v.as_str())
            .map(str::to_string)
    }
}

/// What kind of form widget a parameter needs, and how to validate it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldKind {
    Text,
    Url,
    Number,
    Bool,
    /// One of a fixed set of choices.
    Enum(&'static [&'static str]),
    /// Name of an item field; the overlay offers the key-union picker.
    FieldName,
    /// List of filter rule expressions (`field op value`), one per line.
    RuleList,
}

/// One parameter in a module's schema.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldSpec {
    pub name: &'static str,
    pub label: &'static str,
    pub kind: FieldKind,
    pub required: bool,
    pub default: Option<serde_json::Value>,
    /// Whether `${name}` pipe-param references are expanded in this field.
    /// Off for fields whose own syntax uses `${...}` (regex replacements).
    pub interpolate: bool,
}

impl FieldSpec {
    pub const fn required(name: &'static str, label: &'static str, kind: FieldKind) -> Self {
        Self {
            name,
            label,
            kind,
            required: true,
            default: None,
            interpolate: true,
        }
    }

    pub const fn optional(name: &'static str, label: &'static str, kind: FieldKind) -> Self {
        Self {
            name,
            label,
            kind,
            required: false,
            default: None,
            interpolate: true,
        }
    }

    pub fn with_default(mut self, default: impl Into<serde_json::Value>) -> Self {
        self.default = Some(default.into());
        self
    }

    pub fn no_interpolation(mut self) -> Self {
        self.interpolate = false;
        self
    }
}

/// The full parameter schema for one module kind.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParamSchema {
    pub fields: Vec<FieldSpec>,
}

impl ParamSchema {
    pub fn new(fields: Vec<FieldSpec>) -> Self {
        Self { fields }
    }

    /// Validate `params` against this schema.
    ///
    /// Errors block evaluation (missing required field, wrong type, unknown
    /// enum variant). Unknown params are warnings, not errors: pipe files
    /// from newer versions should load with a complaint, not fail.
    pub fn validate(&self, params: &Params) -> Validation {
        let mut v = Validation::default();
        for field in &self.fields {
            match params.get(field.name) {
                None => {
                    if field.required && field.default.is_none() {
                        v.errors
                            .push(format!("missing required param `{}`", field.name));
                    }
                }
                Some(value) => {
                    if let Err(msg) = check_type(field, value) {
                        v.errors.push(format!("param `{}`: {msg}", field.name));
                    }
                }
            }
        }
        for key in params.0.keys() {
            if !self.fields.iter().any(|f| f.name == key) {
                v.warnings.push(format!("unknown param `{key}` (ignored)"));
            }
        }
        v
    }
}

fn check_type(field: &FieldSpec, value: &serde_json::Value) -> Result<(), String> {
    use serde_json::Value;
    match (&field.kind, value) {
        (FieldKind::Text | FieldKind::Url | FieldKind::FieldName, Value::String(_)) => Ok(()),
        (FieldKind::Number, Value::Number(_)) => Ok(()),
        (FieldKind::Bool, Value::Bool(_)) => Ok(()),
        (FieldKind::Enum(variants), Value::String(s)) => {
            if variants.contains(&s.as_str()) {
                Ok(())
            } else {
                Err(format!("`{s}` is not one of {variants:?}"))
            }
        }
        (FieldKind::RuleList, Value::Array(rules)) => {
            if rules.iter().all(Value::is_string) {
                Ok(())
            } else {
                Err("rule list must contain only strings".to_string())
            }
        }
        (kind, other) => Err(format!("expected {kind:?}, got {other}")),
    }
}

/// Result of validating params against a schema.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Validation {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

impl Validation {
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema() -> ParamSchema {
        ParamSchema::new(vec![
            FieldSpec::required("url", "Feed URL", FieldKind::Url),
            FieldSpec::optional("limit", "Max items", FieldKind::Number).with_default(20),
            FieldSpec::optional("order", "Order", FieldKind::Enum(&["asc", "desc"])),
            FieldSpec::optional("rules", "Rules", FieldKind::RuleList),
        ])
    }

    #[test]
    fn valid_params_pass() {
        let params = Params::new()
            .with("url", "https://x.dev/feed")
            .with("limit", 5)
            .with("order", "desc")
            .with("rules", json!(["score > 100"]));
        let v = schema().validate(&params);
        assert!(v.is_ok(), "{:?}", v.errors);
        assert!(v.warnings.is_empty());
    }

    #[test]
    fn missing_required_is_error() {
        let v = schema().validate(&Params::new());
        assert_eq!(v.errors, vec!["missing required param `url`"]);
    }

    #[test]
    fn wrong_type_and_bad_enum_are_errors() {
        let params = Params::new().with("url", 7).with("order", "sideways");
        let v = schema().validate(&params);
        assert_eq!(v.errors.len(), 2, "{:?}", v.errors);
    }

    #[test]
    fn unknown_param_is_warning_only() {
        let params = Params::new()
            .with("url", "https://x.dev/f")
            .with("bogus", 1);
        let v = schema().validate(&params);
        assert!(v.is_ok());
        assert_eq!(v.warnings, vec!["unknown param `bogus` (ignored)"]);
    }

    #[test]
    fn str_or_default_falls_back_to_schema() {
        let schema = ParamSchema::new(vec![
            FieldSpec::optional("sep", "Separator", FieldKind::Text).with_default(", "),
        ]);
        let params = Params::new();
        assert_eq!(params.str_or_default("sep", &schema).as_deref(), Some(", "));
        let params = Params::new().with("sep", " | ");
        assert_eq!(
            params.str_or_default("sep", &schema).as_deref(),
            Some(" | ")
        );
    }

    #[test]
    fn params_serde_round_trip() {
        let params = Params::new().with("url", "https://x.dev").with("limit", 3);
        let s = serde_json::to_string(&params).unwrap();
        assert_eq!(serde_json::from_str::<Params>(&s).unwrap(), params);
    }
}
