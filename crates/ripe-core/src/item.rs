//! The data that flows on wires: items (JSON records) and port values.

use std::hash::{Hash, Hasher};

use serde::{Deserialize, Serialize};

/// An item is a JSON object: a feed entry or generic record. RSS fields are
/// conventional keys (`title`, `link`, `description`, `pubDate`, `author`).
///
/// Newtype (not a bare alias) so helpers — dotted-path lookup, coercion,
/// canonical content hashing — hang off it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Item(pub serde_json::Map<String, serde_json::Value>);

impl Item {
    pub fn new() -> Self {
        Self::default()
    }

    /// Look up a field by dotted path (`item.pubDate` and `pubDate` are the
    /// same field: a leading `item.` segment is stripped).
    pub fn get(&self, path: &str) -> Option<&serde_json::Value> {
        let path = path.strip_prefix("item.").unwrap_or(path);
        let mut segments = path.split('.');
        let mut current = self.0.get(segments.next()?)?;
        for segment in segments {
            current = current.as_object()?.get(segment)?;
        }
        Some(current)
    }

    /// String view of a field: strings come back as-is, other scalars are
    /// rendered as JSON.
    pub fn get_str(&self, path: &str) -> Option<String> {
        match self.get(path)? {
            serde_json::Value::String(s) => Some(s.clone()),
            other => Some(other.to_string()),
        }
    }

    pub fn set(&mut self, key: impl Into<String>, value: impl Into<serde_json::Value>) {
        self.0.insert(key.into(), value.into());
    }

    /// Union of top-level keys across a sample of items — used by field-name
    /// pickers, which must cope with items that do not share keys.
    pub fn key_union<'a>(items: impl IntoIterator<Item = &'a Item>) -> Vec<String> {
        let mut keys: Vec<String> = items
            .into_iter()
            .flat_map(|item| item.0.keys().cloned())
            .collect();
        keys.sort();
        keys.dedup();
        keys
    }
}

impl From<serde_json::Map<String, serde_json::Value>> for Item {
    fn from(map: serde_json::Map<String, serde_json::Value>) -> Self {
        Self(map)
    }
}

/// The type of a port, used for connection validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PortType {
    Items,
    Text,
    Number,
    Url,
    Bool,
    Any,
}

impl PortType {
    /// May a value of type `from` flow into a port of type `to`?
    /// Exact matches and `Any` always connect; otherwise the wire is allowed
    /// when a lossless coercion exists (checked again at eval time).
    pub fn accepts(self, from: PortType) -> bool {
        match (from, self) {
            (a, b) if a == b => true,
            (_, PortType::Any) | (PortType::Any, _) => true,
            // Scalars render as text.
            (PortType::Number | PortType::Url | PortType::Bool, PortType::Text) => true,
            // A URL is text with a scheme; parseability is checked at eval.
            (PortType::Text, PortType::Url) => true,
            (PortType::Text, PortType::Number) => true,
            _ => false,
        }
    }
}

/// A value carried on a wire. MVP wires carry `Items` only; the scalar
/// variants are reserved for later scalar wiring (Simple Math, Count).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum PortValue {
    Items(Vec<Item>),
    Text(String),
    Number(f64),
    Url(String),
    Bool(bool),
}

/// Error produced when a value cannot be coerced to a port's type.
#[derive(Debug, thiserror::Error, PartialEq)]
#[error("cannot convert {from:?} to {to:?}: {reason}")]
pub struct CoerceError {
    pub from: PortType,
    pub to: PortType,
    pub reason: String,
}

impl PortValue {
    pub fn port_type(&self) -> PortType {
        match self {
            PortValue::Items(_) => PortType::Items,
            PortValue::Text(_) => PortType::Text,
            PortValue::Number(_) => PortType::Number,
            PortValue::Url(_) => PortType::Url,
            PortValue::Bool(_) => PortType::Bool,
        }
    }

    /// Convert to the requested port type, or explain why that's impossible.
    pub fn coerce_to(&self, to: PortType) -> Result<PortValue, CoerceError> {
        let from = self.port_type();
        let err = |reason: &str| CoerceError {
            from,
            to,
            reason: reason.to_string(),
        };
        if from == to || to == PortType::Any {
            return Ok(self.clone());
        }
        match (self, to) {
            (PortValue::Number(n), PortType::Text) => Ok(PortValue::Text(format_number(*n))),
            (PortValue::Url(u), PortType::Text) => Ok(PortValue::Text(u.clone())),
            (PortValue::Bool(b), PortType::Text) => Ok(PortValue::Text(b.to_string())),
            (PortValue::Text(s), PortType::Number) => s
                .trim()
                .parse::<f64>()
                .map(PortValue::Number)
                .map_err(|e| err(&format!("not a number: {e}"))),
            (PortValue::Text(s), PortType::Url) => {
                let trimmed = s.trim();
                if trimmed.contains("://") {
                    Ok(PortValue::Url(trimmed.to_string()))
                } else {
                    Err(err("not a URL (missing scheme)"))
                }
            }
            _ => Err(err("no conversion defined")),
        }
    }

    /// Number of items if this is an `Items` value.
    pub fn item_count(&self) -> Option<usize> {
        match self {
            PortValue::Items(items) => Some(items.len()),
            _ => None,
        }
    }
}

/// Render a number the way users expect in text fields: integers lose the
/// trailing `.0`.
fn format_number(n: f64) -> String {
    if n.fract() == 0.0 && n.is_finite() && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        n.to_string()
    }
}

/// Static description of one input or output port on a module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortSpec {
    pub name: &'static str,
    pub ty: PortType,
    pub required: bool,
    /// A variadic port accepts any number of incoming wires (e.g. Union).
    pub variadic: bool,
}

impl PortSpec {
    pub const fn required(name: &'static str, ty: PortType) -> Self {
        Self {
            name,
            ty,
            required: true,
            variadic: false,
        }
    }

    pub const fn optional(name: &'static str, ty: PortType) -> Self {
        Self {
            name,
            ty,
            required: false,
            variadic: false,
        }
    }

    pub const fn variadic(name: &'static str, ty: PortType) -> Self {
        Self {
            name,
            ty,
            required: true,
            variadic: true,
        }
    }
}

/// Stable content hash of any serializable value, computed over its
/// canonical JSON bytes. `serde_json::Map` is BTreeMap-backed (the
/// `preserve_order` feature is deliberately off), so keys serialize sorted
/// and the bytes are canonical.
///
/// In-process only: `DefaultHasher` is not guaranteed stable across Rust
/// versions, and the memo cache never outlives the process.
pub fn content_hash<T: Serialize>(value: &T) -> u64 {
    let bytes = serde_json::to_vec(value).expect("value must serialize to JSON");
    let mut hasher = std::hash::DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn item(v: serde_json::Value) -> Item {
        Item(v.as_object().unwrap().clone())
    }

    #[test]
    fn dotted_get_and_item_prefix() {
        let it = item(json!({"title": "hi", "meta": {"score": 42}}));
        assert_eq!(it.get("title"), Some(&json!("hi")));
        assert_eq!(it.get("item.title"), Some(&json!("hi")));
        assert_eq!(it.get("meta.score"), Some(&json!(42)));
        assert_eq!(it.get("meta.missing"), None);
        assert_eq!(it.get("title.nested"), None);
    }

    #[test]
    fn get_str_renders_scalars() {
        let it = item(json!({"s": "text", "n": 3, "b": true}));
        assert_eq!(it.get_str("s").as_deref(), Some("text"));
        assert_eq!(it.get_str("n").as_deref(), Some("3"));
        assert_eq!(it.get_str("b").as_deref(), Some("true"));
    }

    #[test]
    fn key_union_dedupes_and_sorts() {
        let a = item(json!({"title": 1, "link": 2}));
        let b = item(json!({"title": 1, "score": 3}));
        assert_eq!(Item::key_union([&a, &b]), vec!["link", "score", "title"]);
    }

    #[test]
    fn coercions() {
        assert_eq!(
            PortValue::Number(3.0).coerce_to(PortType::Text),
            Ok(PortValue::Text("3".into()))
        );
        assert_eq!(
            PortValue::Number(3.5).coerce_to(PortType::Text),
            Ok(PortValue::Text("3.5".into()))
        );
        assert_eq!(
            PortValue::Text(" 42 ".into()).coerce_to(PortType::Number),
            Ok(PortValue::Number(42.0))
        );
        assert_eq!(
            PortValue::Text("https://x.dev/feed".into()).coerce_to(PortType::Url),
            Ok(PortValue::Url("https://x.dev/feed".into()))
        );
        assert!(
            PortValue::Text("nope".into())
                .coerce_to(PortType::Url)
                .is_err()
        );
        assert!(
            PortValue::Text("abc".into())
                .coerce_to(PortType::Number)
                .is_err()
        );
        assert!(
            PortValue::Items(vec![])
                .coerce_to(PortType::Number)
                .is_err()
        );
    }

    #[test]
    fn port_type_accepts() {
        assert!(PortType::Items.accepts(PortType::Items));
        assert!(PortType::Any.accepts(PortType::Items));
        assert!(PortType::Text.accepts(PortType::Number));
        assert!(PortType::Url.accepts(PortType::Text));
        assert!(!PortType::Items.accepts(PortType::Text));
        assert!(!PortType::Number.accepts(PortType::Items));
    }

    #[test]
    fn content_hash_is_key_order_independent() {
        // Maps are BTreeMap-backed, so insertion order cannot leak into the hash.
        let mut a = Item::new();
        a.set("x", 1);
        a.set("y", 2);
        let mut b = Item::new();
        b.set("y", 2);
        b.set("x", 1);
        assert_eq!(content_hash(&a), content_hash(&b));
        b.set("x", 3);
        assert_ne!(content_hash(&a), content_hash(&b));
    }

    #[test]
    fn port_value_serde_round_trip() {
        let v = PortValue::Items(vec![item(json!({"title": "a"}))]);
        let s = serde_json::to_string(&v).unwrap();
        assert_eq!(serde_json::from_str::<PortValue>(&s).unwrap(), v);
    }
}
