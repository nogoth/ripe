//! The action table: every Normal-mode command in one place.
//!
//! One catalog drives four things that must never disagree: key resolution in
//! `update`, the help overlay, the status-line hints, and the command
//! palette. Config-file remaps (see [`crate::config`]) rewrite the bindings,
//! and every consumer reads labels back through [`Keymap`], so a remapped key
//! shows up remapped everywhere.
//!
//! Modal capture keys (path prompt, param overlay, insert-pending letters,
//! quit-guard, the connect state machine) are deliberately *not* actions:
//! those modes consume the raw key stream. Likewise the harness-level keys
//! that must work in every mode (Tab, `?`, Esc, Ctrl-C/S/O) are translated in
//! `event.rs` before the keymap is consulted; they appear in the catalog as
//! `fixed` so help and the palette can list them, but they cannot be remapped.

use std::collections::BTreeMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Where a binding is active. `Global` works in any pane (Normal mode);
/// pane contexts win over `Global` when both bind the same key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Context {
    Global,
    Canvas,
    Preview,
}

impl Context {
    /// Section header used by the help overlay and the command palette.
    pub fn heading(self) -> &'static str {
        match self {
            Context::Global => "GLOBAL",
            Context::Canvas => "CANVAS",
            Context::Preview => "PREVIEW",
        }
    }
}

/// Every dispatchable Normal-mode command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    // Global
    Quit,
    Save,
    Open,
    RunAll,
    RunToSelected,
    Undo,
    Redo,
    CommandPalette,
    Help,
    NextPane,
    // Canvas
    Insert,
    DeleteNode,
    DeleteEdge,
    Connect,
    EditParams,
    StepNext,
    StepPrev,
    LateralLeft,
    LateralRight,
    // Preview
    PrevTab,
    NextTab,
    ToggleAutoRefresh,
    ScrollDown,
    ScrollUp,
    ScrollTop,
    ScrollBottom,
}

/// A key shape a binding matches: a code plus whether Ctrl is held. Shift is
/// carried by the character itself (`R` vs `r`), so it is not tracked; Alt
/// chords are rejected wholesale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyPattern {
    pub code: KeyCode,
    pub ctrl: bool,
}

impl KeyPattern {
    pub const fn plain(code: KeyCode) -> Self {
        KeyPattern { code, ctrl: false }
    }

    pub const fn ctrl(code: KeyCode) -> Self {
        KeyPattern { code, ctrl: true }
    }

    pub fn matches(&self, key: &KeyEvent) -> bool {
        key.code == self.code
            && key.modifiers.contains(KeyModifiers::CONTROL) == self.ctrl
            && !key.modifiers.contains(KeyModifiers::ALT)
    }

    /// Human-readable label: `q`, `R`, `Ctrl-r`, `Enter`, `[`.
    pub fn label(&self) -> String {
        let base = match self.code {
            KeyCode::Char(' ') => "Space".to_string(),
            KeyCode::Char(c) => c.to_string(),
            KeyCode::Enter => "Enter".to_string(),
            KeyCode::Esc => "Esc".to_string(),
            KeyCode::Tab => "Tab".to_string(),
            KeyCode::Up => "Up".to_string(),
            KeyCode::Down => "Down".to_string(),
            KeyCode::Left => "Left".to_string(),
            KeyCode::Right => "Right".to_string(),
            KeyCode::Home => "Home".to_string(),
            KeyCode::End => "End".to_string(),
            other => format!("{other:?}"),
        };
        if self.ctrl {
            format!("Ctrl-{base}")
        } else {
            base
        }
    }
}

/// One catalog row: an action, where it lives, what to call it, and its keys.
pub struct ActionInfo {
    pub action: Action,
    pub context: Context,
    /// Verb phrase shown in help and the command palette.
    pub name: &'static str,
    /// Identifier used in the config file's `keys` table.
    pub config_key: &'static str,
    pub default: KeyPattern,
    /// A non-remappable synonym (arrow keys, Home/End). Never shown as the
    /// primary label.
    pub alias: Option<KeyPattern>,
    /// `true` for keys translated ahead of the keymap in `event.rs`; listed
    /// for discoverability but rejected by [`Keymap::with_overrides`].
    pub fixed: bool,
}

const fn row(
    action: Action,
    context: Context,
    name: &'static str,
    config_key: &'static str,
    default: KeyPattern,
) -> ActionInfo {
    ActionInfo {
        action,
        context,
        name,
        config_key,
        default,
        alias: None,
        fixed: false,
    }
}

const fn row_alias(
    action: Action,
    context: Context,
    name: &'static str,
    config_key: &'static str,
    default: KeyPattern,
    alias: KeyPattern,
) -> ActionInfo {
    ActionInfo {
        action,
        context,
        name,
        config_key,
        default,
        alias: Some(alias),
        fixed: false,
    }
}

const fn fixed_row(
    action: Action,
    context: Context,
    name: &'static str,
    config_key: &'static str,
    default: KeyPattern,
) -> ActionInfo {
    ActionInfo {
        action,
        context,
        name,
        config_key,
        default,
        alias: None,
        fixed: true,
    }
}

/// The full command catalog, in display order.
pub const CATALOG: &[ActionInfo] = &[
    // --- global -----------------------------------------------------------
    row(
        Action::RunAll,
        Context::Global,
        "Run all",
        "run_all",
        KeyPattern::plain(KeyCode::Char('r')),
    ),
    row(
        Action::RunToSelected,
        Context::Global,
        "Run to selected",
        "run_to_selected",
        KeyPattern::plain(KeyCode::Char('R')),
    ),
    row(
        Action::Undo,
        Context::Global,
        "Undo",
        "undo",
        KeyPattern::plain(KeyCode::Char('u')),
    ),
    row(
        Action::Redo,
        Context::Global,
        "Redo",
        "redo",
        KeyPattern::ctrl(KeyCode::Char('r')),
    ),
    row(
        Action::CommandPalette,
        Context::Global,
        "Command palette",
        "command_palette",
        KeyPattern::plain(KeyCode::Char(':')),
    ),
    fixed_row(
        Action::Save,
        Context::Global,
        "Save pipe",
        "save",
        KeyPattern::ctrl(KeyCode::Char('s')),
    ),
    fixed_row(
        Action::Open,
        Context::Global,
        "Open pipe",
        "open",
        KeyPattern::ctrl(KeyCode::Char('o')),
    ),
    fixed_row(
        Action::NextPane,
        Context::Global,
        "Next pane",
        "next_pane",
        KeyPattern::plain(KeyCode::Tab),
    ),
    fixed_row(
        Action::Help,
        Context::Global,
        "Toggle help",
        "help",
        KeyPattern::plain(KeyCode::Char('?')),
    ),
    row(
        Action::Quit,
        Context::Global,
        "Quit",
        "quit",
        KeyPattern::plain(KeyCode::Char('q')),
    ),
    // --- canvas -----------------------------------------------------------
    row(
        Action::Insert,
        Context::Canvas,
        "Insert node",
        "insert",
        KeyPattern::plain(KeyCode::Char('a')),
    ),
    row(
        Action::DeleteNode,
        Context::Canvas,
        "Delete node",
        "delete_node",
        KeyPattern::plain(KeyCode::Char('d')),
    ),
    row(
        Action::DeleteEdge,
        Context::Canvas,
        "Delete edge",
        "delete_edge",
        KeyPattern::plain(KeyCode::Char('x')),
    ),
    row(
        Action::Connect,
        Context::Canvas,
        "Connect from selected",
        "connect",
        KeyPattern::plain(KeyCode::Char('c')),
    ),
    row(
        Action::EditParams,
        Context::Canvas,
        "Edit params",
        "edit_params",
        KeyPattern::plain(KeyCode::Enter),
    ),
    row_alias(
        Action::StepNext,
        Context::Canvas,
        "Select downstream",
        "step_next",
        KeyPattern::plain(KeyCode::Char('j')),
        KeyPattern::plain(KeyCode::Down),
    ),
    row_alias(
        Action::StepPrev,
        Context::Canvas,
        "Select upstream",
        "step_prev",
        KeyPattern::plain(KeyCode::Char('k')),
        KeyPattern::plain(KeyCode::Up),
    ),
    row_alias(
        Action::LateralLeft,
        Context::Canvas,
        "Select left branch",
        "lateral_left",
        KeyPattern::plain(KeyCode::Char('h')),
        KeyPattern::plain(KeyCode::Left),
    ),
    row_alias(
        Action::LateralRight,
        Context::Canvas,
        "Select right branch",
        "lateral_right",
        KeyPattern::plain(KeyCode::Char('l')),
        KeyPattern::plain(KeyCode::Right),
    ),
    // --- preview ----------------------------------------------------------
    row(
        Action::PrevTab,
        Context::Preview,
        "Previous preview tab",
        "prev_tab",
        KeyPattern::plain(KeyCode::Char('[')),
    ),
    row(
        Action::NextTab,
        Context::Preview,
        "Next preview tab",
        "next_tab",
        KeyPattern::plain(KeyCode::Char(']')),
    ),
    row(
        Action::ToggleAutoRefresh,
        Context::Preview,
        "Toggle auto-refresh",
        "auto_refresh",
        KeyPattern::plain(KeyCode::Char('a')),
    ),
    row_alias(
        Action::ScrollDown,
        Context::Preview,
        "Scroll down",
        "scroll_down",
        KeyPattern::plain(KeyCode::Char('j')),
        KeyPattern::plain(KeyCode::Down),
    ),
    row_alias(
        Action::ScrollUp,
        Context::Preview,
        "Scroll up",
        "scroll_up",
        KeyPattern::plain(KeyCode::Char('k')),
        KeyPattern::plain(KeyCode::Up),
    ),
    row_alias(
        Action::ScrollTop,
        Context::Preview,
        "Scroll to top",
        "scroll_top",
        KeyPattern::plain(KeyCode::Char('g')),
        KeyPattern::plain(KeyCode::Home),
    ),
    row_alias(
        Action::ScrollBottom,
        Context::Preview,
        "Scroll to bottom",
        "scroll_bottom",
        KeyPattern::plain(KeyCode::Char('G')),
        KeyPattern::plain(KeyCode::End),
    ),
];

/// One live binding: catalog row + the (possibly remapped) key.
struct Binding {
    pattern: KeyPattern,
    alias: Option<KeyPattern>,
    context: Context,
    action: Action,
}

/// The resolved keymap: catalog defaults with config overrides applied.
pub struct Keymap {
    bindings: Vec<Binding>,
}

impl Default for Keymap {
    fn default() -> Self {
        Keymap {
            bindings: CATALOG
                .iter()
                .map(|info| Binding {
                    pattern: info.default,
                    alias: info.alias,
                    context: info.context,
                    action: info.action,
                })
                .collect(),
        }
    }
}

impl Keymap {
    /// Build from config `keys` overrides (`config_key` → key spec). Invalid
    /// entries never fail the load: each produces a warning and is skipped.
    pub fn with_overrides(overrides: &BTreeMap<String, String>) -> (Self, Vec<String>) {
        let mut keymap = Keymap::default();
        let mut warnings = Vec::new();
        for (name, spec) in overrides {
            let Some(idx) = CATALOG.iter().position(|i| i.config_key == name) else {
                warnings.push(format!("config keys: unknown action `{name}`"));
                continue;
            };
            if CATALOG[idx].fixed {
                warnings.push(format!("config keys: `{name}` is not remappable"));
                continue;
            }
            let Some(pattern) = parse_key(spec) else {
                warnings.push(format!("config keys: cannot parse `{spec}` for `{name}`"));
                continue;
            };
            keymap.bindings[idx].pattern = pattern;
        }
        // Same key claimed twice in one scope: the first (catalog order) wins
        // at resolve time; say so rather than silently shadowing.
        for (i, a) in keymap.bindings.iter().enumerate() {
            for b in &keymap.bindings[i + 1..] {
                let overlap = a.context == b.context
                    || a.context == Context::Global
                    || b.context == Context::Global;
                if overlap && a.pattern == b.pattern {
                    warnings.push(format!(
                        "config keys: `{}` and `{}` both bind {}",
                        CATALOG[i].config_key,
                        CATALOG
                            .iter()
                            .find(|c| c.action == b.action)
                            .map_or("?", |c| c.config_key),
                        a.pattern.label(),
                    ));
                }
            }
        }
        (keymap, warnings)
    }

    /// The action for `key` given the focused pane's context. Pane bindings
    /// are consulted before global ones so e.g. `a` can mean Insert on the
    /// canvas and auto-refresh in the preview.
    pub fn resolve(&self, key: &KeyEvent, context: Option<Context>) -> Option<Action> {
        let hit = |b: &&Binding| {
            b.pattern.matches(key) || b.alias.as_ref().is_some_and(|a| a.matches(key))
        };
        if let Some(ctx) = context
            && let Some(b) = self.bindings.iter().filter(|b| b.context == ctx).find(hit)
        {
            return Some(b.action);
        }
        self.bindings
            .iter()
            .filter(|b| b.context == Context::Global)
            .find(hit)
            .map(|b| b.action)
    }

    /// Display label for an action's current primary key.
    pub fn label(&self, action: Action) -> String {
        self.bindings
            .iter()
            .find(|b| b.action == action)
            .map_or_else(String::new, |b| b.pattern.label())
    }
}

/// Parse a config key spec: `q`, `R`, `ctrl-r`, `enter`, `space`, `[`.
/// `shift-` is rejected — uppercase the character instead.
pub fn parse_key(spec: &str) -> Option<KeyPattern> {
    let spec = spec.trim();
    let (ctrl, rest) = match spec
        .strip_prefix("ctrl-")
        .or_else(|| spec.strip_prefix("ctrl+"))
        .or_else(|| spec.strip_prefix("Ctrl-"))
        .or_else(|| spec.strip_prefix("Ctrl+"))
    {
        Some(rest) => (true, rest),
        None => (false, spec),
    };
    let code = match rest.to_ascii_lowercase().as_str() {
        "enter" | "return" => KeyCode::Enter,
        "space" => KeyCode::Char(' '),
        "tab" => KeyCode::Tab,
        "esc" | "escape" => KeyCode::Esc,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        _ => {
            let mut chars = rest.chars();
            let ch = chars.next()?;
            if chars.next().is_some() {
                return None; // multi-char and not a named key
            }
            KeyCode::Char(ch)
        }
    };
    Some(KeyPattern { code, ctrl })
}

/// Case-insensitive subsequence match of `query` in `name`, scored so tighter
/// and earlier matches rank first. `None` when `query` is not a subsequence.
pub fn fuzzy_score(query: &str, name: &str) -> Option<u32> {
    let name: Vec<char> = name.to_lowercase().chars().collect();
    let query: Vec<char> = query.to_lowercase().chars().collect();
    if query.is_empty() {
        return Some(0);
    }
    let mut qi = 0;
    let mut first = 0usize;
    let mut last = 0usize;
    for (ni, &nc) in name.iter().enumerate() {
        if qi < query.len() && nc == query[qi] {
            if qi == 0 {
                first = ni;
            }
            last = ni;
            qi += 1;
        }
    }
    if qi < query.len() {
        return None;
    }
    // Span dominates; start position breaks ties. Tighter spans (exacter
    // matches) always beat looser ones regardless of where they start.
    let span = (last - first) as u32;
    Some((span << 8) | (first as u32).min(255))
}

/// Catalog rows matching `query`, best score first (stable within a score).
pub fn filtered_actions(query: &str) -> Vec<&'static ActionInfo> {
    let mut scored: Vec<(u32, &'static ActionInfo)> = CATALOG
        .iter()
        .filter_map(|info| fuzzy_score(query, info.name).map(|s| (s, info)))
        .collect();
    scored.sort_by_key(|(s, _)| *s);
    scored.into_iter().map(|(_, info)| info).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::empty())
    }

    fn ctrl_key(ch: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL)
    }

    #[test]
    fn defaults_resolve_by_context() {
        let km = Keymap::default();
        // `a` is Insert on the canvas, auto-refresh in the preview, and
        // nothing globally.
        let a = key(KeyCode::Char('a'));
        assert_eq!(km.resolve(&a, Some(Context::Canvas)), Some(Action::Insert));
        assert_eq!(
            km.resolve(&a, Some(Context::Preview)),
            Some(Action::ToggleAutoRefresh)
        );
        assert_eq!(km.resolve(&a, None), None);
        // `q` quits from every context (global fallback).
        let q = key(KeyCode::Char('q'));
        for ctx in [None, Some(Context::Canvas), Some(Context::Preview)] {
            assert_eq!(km.resolve(&q, ctx), Some(Action::Quit));
        }
        // Ctrl-r is Redo, distinct from plain r.
        assert_eq!(km.resolve(&ctrl_key('r'), None), Some(Action::Redo));
        assert_eq!(
            km.resolve(&key(KeyCode::Char('r')), None),
            Some(Action::RunAll)
        );
        // Arrow aliases work where defined.
        assert_eq!(
            km.resolve(&key(KeyCode::Down), Some(Context::Canvas)),
            Some(Action::StepNext)
        );
    }

    #[test]
    fn parse_key_covers_the_spec_grammar() {
        assert_eq!(parse_key("q"), Some(KeyPattern::plain(KeyCode::Char('q'))));
        assert_eq!(parse_key("R"), Some(KeyPattern::plain(KeyCode::Char('R'))));
        assert_eq!(
            parse_key("ctrl-r"),
            Some(KeyPattern::ctrl(KeyCode::Char('r')))
        );
        assert_eq!(parse_key("enter"), Some(KeyPattern::plain(KeyCode::Enter)));
        assert_eq!(parse_key("["), Some(KeyPattern::plain(KeyCode::Char('['))));
        assert_eq!(
            parse_key("space"),
            Some(KeyPattern::plain(KeyCode::Char(' ')))
        );
        assert_eq!(parse_key("bogus"), None);
        assert_eq!(parse_key(""), None);
    }

    #[test]
    fn overrides_remap_and_reject() {
        let mut overrides = BTreeMap::new();
        overrides.insert("run_all".to_string(), "e".to_string());
        overrides.insert("save".to_string(), "w".to_string()); // fixed
        overrides.insert("no_such".to_string(), "z".to_string());
        overrides.insert("undo".to_string(), "!!bad!!".to_string());
        let (km, warnings) = Keymap::with_overrides(&overrides);

        // The remap took: e runs, r no longer does.
        assert_eq!(
            km.resolve(&key(KeyCode::Char('e')), None),
            Some(Action::RunAll)
        );
        assert_eq!(km.resolve(&key(KeyCode::Char('r')), None), None);
        // Undo kept its default after the bad spec.
        assert_eq!(
            km.resolve(&key(KeyCode::Char('u')), None),
            Some(Action::Undo)
        );
        // One warning each: fixed, unknown, unparseable.
        assert_eq!(warnings.len(), 3, "warnings: {warnings:?}");
        // Labels reflect the remap.
        assert_eq!(km.label(Action::RunAll), "e");
    }

    #[test]
    fn conflicting_override_warns() {
        let mut overrides = BTreeMap::new();
        overrides.insert("undo".to_string(), "d".to_string()); // canvas `d` = delete
        let (_, warnings) = Keymap::with_overrides(&overrides);
        assert!(
            warnings.iter().any(|w| w.contains("both bind")),
            "warnings: {warnings:?}"
        );
    }

    #[test]
    fn fuzzy_prefers_tight_early_matches() {
        // Exact prefix beats scattered subsequence.
        let tight = fuzzy_score("del", "Delete node").unwrap();
        let loose = fuzzy_score("del", "Do not delay").unwrap();
        assert!(tight < loose);
        // Non-subsequence is rejected.
        assert_eq!(fuzzy_score("zz", "Delete node"), None);
        // Empty query matches everything equally.
        assert_eq!(fuzzy_score("", "anything"), Some(0));
        // Case-insensitive.
        assert!(fuzzy_score("RUN", "Run all").is_some());
    }

    #[test]
    fn filtered_actions_ranks_and_filters() {
        let all = filtered_actions("");
        assert_eq!(all.len(), CATALOG.len());
        let runs = filtered_actions("run");
        assert!(!runs.is_empty());
        assert_eq!(runs[0].action, Action::RunAll, "prefix match ranks first");
        assert!(
            runs.iter().all(|i| fuzzy_score("run", i.name).is_some()),
            "everything listed actually matches"
        );
    }
}
