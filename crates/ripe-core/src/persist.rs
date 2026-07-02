//! Saving and loading pipes: pretty-printed JSON on disk, `.pipe` by
//! convention (never enforced).
//!
//! Loading is deliberately forgiving. Hard errors are reserved for files
//! that cannot be opened as a pipe at all: unreadable, unparseable, saved
//! by a newer ripe, or structurally invalid per [`Pipe::validate`].
//! Everything a user could fix in the editor — unknown fields, unknown or
//! missing node params — surfaces as warnings so the pipe still opens.

use std::path::{Path, PathBuf};

use crate::graph::{PIPE_FORMAT_VERSION, Pipe};
use crate::module::Registry;

/// Why a pipe file could not be saved or loaded.
#[derive(Debug, thiserror::Error)]
pub enum PersistError {
    #[error("{}: {source}", .path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// Broken JSON syntax, or JSON without a pipe's shape.
    #[error("{} is not a valid pipe file: {source}", .path.display())]
    Malformed {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error(
        "{} has format version {found}; this ripe reads up to {PIPE_FORMAT_VERSION} \
         (the file was saved by a newer ripe)",
        .path.display()
    )]
    NewerVersion { path: PathBuf, found: u64 },
    #[error("cannot migrate {} from format version {from}: {message}", .path.display())]
    Migration {
        path: PathBuf,
        from: u32,
        message: String,
    },
    /// [`Pipe::validate`] rejected the graph.
    #[error("{} is not a valid pipe:\n{}", .path.display(), .errors.join("\n"))]
    Invalid { path: PathBuf, errors: Vec<String> },
}

/// A successfully loaded pipe plus the non-fatal complaints found on the way.
#[derive(Debug)]
pub struct Loaded {
    pub pipe: Pipe,
    /// One message per problem: unknown top-level fields, and per-node
    /// param-schema findings prefixed with the node's id and kind.
    pub warnings: Vec<String>,
}

/// The fields `Pipe` deserializes. Serde skips unknown fields silently, so
/// warning about them means comparing keys against this list by hand; keep
/// it in sync with the struct (a test below checks).
const PIPE_FIELDS: &[&str] = &["version", "name", "params", "nodes", "edges", "next_id"];

/// Write `pipe` to `path` as pretty-printed JSON.
pub fn save_pipe(pipe: &Pipe, path: &Path) -> Result<(), PersistError> {
    // Pipe is plain data (string keys, no NaN-capable numbers), so
    // serialization cannot fail.
    let mut json = serde_json::to_string_pretty(pipe).expect("Pipe must serialize to JSON");
    json.push('\n');
    std::fs::write(path, json).map_err(|source| PersistError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Read a pipe from `path`, migrating older format versions and validating
/// the result against `registry`.
///
/// Param-schema findings — including *errors* such as a missing required
/// param — come back as [`Loaded::warnings`], not [`PersistError`]s: an
/// incomplete node must not block opening the pipe for editing, and the
/// engine already reports such nodes as `Err`/`Unready` at eval time.
pub fn load_pipe(path: &Path, registry: &Registry) -> Result<Loaded, PersistError> {
    let text = std::fs::read_to_string(path).map_err(|source| PersistError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut value: serde_json::Value =
        serde_json::from_str(&text).map_err(|source| PersistError::Malformed {
            path: path.to_path_buf(),
            source,
        })?;

    // The version gate and migrations work on the raw JSON: deserializing
    // first would reject exactly the old shapes a migration exists to fix.
    match value.get("version").and_then(serde_json::Value::as_u64) {
        Some(found) if found > u64::from(PIPE_FORMAT_VERSION) => {
            return Err(PersistError::NewerVersion {
                path: path.to_path_buf(),
                found,
            });
        }
        Some(found) if found < u64::from(PIPE_FORMAT_VERSION) => {
            migrate(&mut value, found as u32).map_err(|message| PersistError::Migration {
                path: path.to_path_buf(),
                from: found as u32,
                message,
            })?;
        }
        // Current version, or a missing/garbled version field; the latter
        // fails deserialization below with a precise message.
        _ => {}
    }

    let mut warnings = Vec::new();
    if let Some(object) = value.as_object() {
        for key in object.keys() {
            if !PIPE_FIELDS.contains(&key.as_str()) {
                warnings.push(format!("unknown field `{key}` (ignored)"));
            }
        }
    }

    let pipe: Pipe = serde_json::from_value(value).map_err(|source| PersistError::Malformed {
        path: path.to_path_buf(),
        source,
    })?;

    let errors = pipe.validate(registry);
    if !errors.is_empty() {
        return Err(PersistError::Invalid {
            path: path.to_path_buf(),
            errors,
        });
    }

    for node in &pipe.nodes {
        let Some(module) = registry.get(&node.kind) else {
            continue; // unreachable: validate() rejects unknown kinds
        };
        let validation = module.param_schema().validate(&node.params);
        for finding in validation.errors.into_iter().chain(validation.warnings) {
            warnings.push(format!("node {} ({}): {finding}", node.id, node.kind));
        }
    }

    Ok(Loaded { pipe, warnings })
}

/// Rewrite the raw JSON of a pipe saved at `from_version` into the current
/// shape, one version step at a time, before deserialization sees it.
///
/// v1 is the first shipped format, so no steps exist yet; the match below
/// is the slot where the first real step lands once v2 changes the shape.
fn migrate(value: &mut serde_json::Value, from_version: u32) -> Result<(), String> {
    for version in from_version..PIPE_FORMAT_VERSION {
        // Drop the expect() when the first real arm makes this multi-arm.
        #[expect(clippy::match_single_binding, reason = "no migration steps exist yet")]
        match version {
            // When PIPE_FORMAT_VERSION becomes 2, the v1 -> v2 rewrite of
            // `value` goes here:
            // 1 => migrate_v1_to_v2(value)?,
            _ => Err(format!("no migration step from format version {version}"))?,
        }
    }
    // Steps rewrite the payload; the version field itself lands on current.
    if let Some(version) = value.get_mut("version") {
        *version = PIPE_FORMAT_VERSION.into();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::bind::{PipeParam, PipeParamKind};
    use crate::graph::NodeId;
    use crate::params::Params;

    /// Temp file that removes itself. `std::env::temp_dir()` keeps tests
    /// off new dependencies (same approach as logging.rs).
    struct TempFile(PathBuf);

    impl TempFile {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("ripe-persist-{}-{name}", std::process::id()));
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn write(&self, contents: &str) {
            std::fs::write(&self.0, contents).unwrap();
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            std::fs::remove_file(&self.0).ok();
        }
    }

    /// The pipe from docs/mockup.png: HN filtered to the last two days,
    /// scores extracted and thresholded, merged with Lobsters, newest
    /// first, top 20, out as RSS. The feed URL is a pipe param.
    fn mockup_pipe() -> Pipe {
        let mut pipe = Pipe::new("news_pipeline");
        pipe.params.push(
            PipeParam::new("url", PipeParamKind::Url)
                .with_default("https://news.ycombinator.com/rss"),
        );
        let hn = pipe.add_node("fetch_feed", Params::new().with("url", "${url}"));
        let fresh = pipe.add_node(
            "filter",
            Params::new().with("rules", json!(["pubDate > now - 2d"])),
        );
        let score = pipe.add_node(
            "regex",
            Params::new()
                .with("field", "title")
                .with("pattern", r"(?<score>\d+) points")
                .with("mode", "extract"),
        );
        let hot = pipe.add_node(
            "filter",
            Params::new().with("rules", json!(["score > 100"])),
        );
        let lobsters = pipe.add_node(
            "fetch_feed",
            Params::new().with("url", "https://lobste.rs/rss"),
        );
        let union = pipe.add_node("union", Params::new());
        let sort = pipe.add_node(
            "sort",
            Params::new().with("by", "pubDate").with("order", "desc"),
        );
        let limit = pipe.add_node("limit", Params::new().with("n", 20));
        let output = pipe.add_node(
            "output",
            Params::new()
                .with("format", "rss")
                .with("destination", "output.xml"),
        );
        pipe.connect(hn, "out", fresh, "in");
        pipe.connect(fresh, "out", score, "in");
        pipe.connect(score, "out", hot, "in");
        pipe.connect(hot, "out", union, "in");
        pipe.connect(lobsters, "out", union, "in");
        pipe.connect(union, "out", sort, "in");
        pipe.connect(sort, "out", limit, "in");
        pipe.connect(limit, "out", output, "in");
        pipe
    }

    #[test]
    fn save_then_load_round_trips() {
        let registry = Registry::with_builtins();
        let pipe = mockup_pipe();
        let file = TempFile::new("round-trip.pipe");
        save_pipe(&pipe, file.path()).unwrap();
        let loaded = load_pipe(file.path(), &registry).unwrap();
        assert_eq!(loaded.pipe, pipe);
        assert_eq!(loaded.warnings, Vec::<String>::new());
        // next_id survives: adds after a reload must not reuse ids.
        let mut reloaded = loaded.pipe;
        assert_eq!(reloaded.add_node("union", Params::new()), NodeId(10));
    }

    #[test]
    fn unknown_field_and_param_problems_warn_but_load() {
        let file = TempFile::new("warnings.pipe");
        file.write(
            r#"{
                "version": 1,
                "name": "warnings",
                "layout": {"cols": 3},
                "nodes": [
                    {"id": 1, "kind": "union", "params": {"bogus": true}},
                    {"id": 2, "kind": "fetch_feed", "params": {}}
                ],
                "edges": [],
                "next_id": 3
            }"#,
        );
        let loaded = load_pipe(file.path(), &Registry::with_builtins()).unwrap();
        assert_eq!(loaded.pipe.nodes.len(), 2);
        assert_eq!(
            loaded.warnings,
            vec![
                "unknown field `layout` (ignored)".to_string(),
                "node #1 (union): unknown param `bogus` (ignored)".to_string(),
                // A schema *error*, deliberately demoted to a warning here.
                "node #2 (fetch_feed): missing required param `url`".to_string(),
            ]
        );
    }

    #[test]
    fn malformed_json_and_wrong_shape_are_hard_errors() {
        let registry = Registry::with_builtins();
        let file = TempFile::new("malformed.pipe");
        file.write("{ this is not json");
        let err = load_pipe(file.path(), &registry).unwrap_err();
        assert!(matches!(err, PersistError::Malformed { .. }), "{err}");

        file.write(r#"[1, 2, 3]"#); // valid JSON, not a pipe
        let err = load_pipe(file.path(), &registry).unwrap_err();
        assert!(matches!(err, PersistError::Malformed { .. }), "{err}");
    }

    #[test]
    fn missing_file_is_an_io_error() {
        let err = load_pipe(
            Path::new("/no/such/dir/nothing.pipe"),
            &Registry::with_builtins(),
        )
        .unwrap_err();
        assert!(matches!(err, PersistError::Io { .. }), "{err}");
    }

    #[test]
    fn newer_version_is_a_hard_error() {
        let file = TempFile::new("future.pipe");
        file.write(r#"{"version": 999, "name": "f", "nodes": [], "edges": [], "next_id": 1}"#);
        let err = load_pipe(file.path(), &Registry::with_builtins()).unwrap_err();
        assert!(
            matches!(err, PersistError::NewerVersion { found: 999, .. }),
            "{err}"
        );
        assert!(err.to_string().contains("newer ripe"), "{err}");
    }

    #[test]
    fn unmigratable_version_is_a_hard_error() {
        // Version 0 never shipped, so no migration step exists for it.
        let file = TempFile::new("ancient.pipe");
        file.write(r#"{"version": 0, "name": "a", "nodes": [], "edges": [], "next_id": 1}"#);
        let err = load_pipe(file.path(), &Registry::with_builtins()).unwrap_err();
        assert!(
            matches!(err, PersistError::Migration { from: 0, .. }),
            "{err}"
        );
    }

    #[test]
    fn structural_problems_are_hard_errors() {
        let file = TempFile::new("invalid.pipe");
        file.write(
            r#"{
                "version": 1,
                "name": "invalid",
                "nodes": [{"id": 1, "kind": "no_such_kind", "params": {}}],
                "edges": [],
                "next_id": 2
            }"#,
        );
        let err = load_pipe(file.path(), &Registry::with_builtins()).unwrap_err();
        let PersistError::Invalid { errors, .. } = &err else {
            panic!("expected Invalid, got {err}");
        };
        assert!(
            errors.iter().any(|e| e.contains("unknown module kind")),
            "{errors:?}"
        );
    }

    #[test]
    fn pipe_fields_matches_the_struct() {
        // mockup_pipe() has params, so no field is skip-serialized away.
        let value = serde_json::to_value(mockup_pipe()).unwrap();
        let mut keys: Vec<_> = value.as_object().unwrap().keys().cloned().collect();
        keys.sort_unstable();
        let mut known: Vec<_> = PIPE_FIELDS.iter().map(ToString::to_string).collect();
        known.sort_unstable();
        assert_eq!(keys, known, "keep PIPE_FIELDS in sync with Pipe");
    }

    #[test]
    fn representative_pipe_snapshot() {
        let pipe = mockup_pipe();
        assert_eq!(
            pipe.validate(&Registry::with_builtins()),
            Vec::<String>::new()
        );
        insta::assert_snapshot!(serde_json::to_string_pretty(&pipe).unwrap());
    }
}
