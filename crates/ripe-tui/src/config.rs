//! User configuration: `config.json` in the OS config dir (via `directories`,
//! e.g. `~/.config/ripe/config.json` on Linux). JSON to match the pipe file
//! format — one serialization story across the project.
//!
//! ```json
//! {
//!   "theme": "light",
//!   "keys": { "run_all": "e", "undo": "ctrl-z" }
//! }
//! ```
//!
//! Loading never fails the app: a missing file is the default config, and a
//! malformed one logs a warning and falls back to defaults. Unknown fields
//! are ignored so future keys stay backward-compatible.

use std::collections::BTreeMap;
use std::path::PathBuf;

/// Parsed user config. `keys` maps catalog `config_key`s (see
/// [`crate::actions::CATALOG`]) to key specs (`"q"`, `"ctrl-r"`, `"enter"`).
#[derive(Debug, Default, serde::Deserialize)]
#[serde(default)]
pub struct Config {
    pub theme: Option<String>,
    pub keys: BTreeMap<String, String>,
}

/// Where the config file lives, when the platform exposes a config dir.
pub fn config_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "ripe").map(|d| d.config_dir().join("config.json"))
}

/// Load the user config, defaulting on absence and warning (not failing) on
/// unreadable or malformed content.
pub fn load() -> Config {
    let Some(path) = config_path() else {
        return Config::default();
    };
    match std::fs::read_to_string(&path) {
        Ok(text) => parse(&text).unwrap_or_else(|e| {
            tracing::warn!("ignoring malformed {}: {e}", path.display());
            Config::default()
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
        Err(e) => {
            tracing::warn!("cannot read {}: {e}", path.display());
            Config::default()
        }
    }
}

fn parse(text: &str) -> Result<Config, serde_json::Error> {
    serde_json::from_str(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_theme_and_keys() {
        let cfg = parse(r#"{ "theme": "light", "keys": { "run_all": "e" } }"#).unwrap();
        assert_eq!(cfg.theme.as_deref(), Some("light"));
        assert_eq!(cfg.keys.get("run_all").map(String::as_str), Some("e"));
    }

    #[test]
    fn empty_and_unknown_fields_default_cleanly() {
        let cfg = parse("{}").unwrap();
        assert!(cfg.theme.is_none());
        assert!(cfg.keys.is_empty());
        // Unknown fields are tolerated, not fatal.
        assert!(parse(r#"{ "future_flag": true }"#).is_ok());
    }

    #[test]
    fn malformed_json_is_an_error_not_a_panic() {
        assert!(parse("{ nope").is_err());
    }
}
