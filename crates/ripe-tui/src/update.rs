//! The state transition: `update(&mut App, Msg)`. Pure and synchronous —
//! it only mutates the model. Async work (eval) is dispatched here starting
//! in M12; M10 adds all the editing interactions that make ripe actually
//! usable from the keyboard. M11 adds the param-edit overlay.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent};

use ripe_core::EvalReport;
use ripe_core::params::{FieldKind, Params};
use ripe_core::persist::{load_pipe, save_pipe};

use crate::app::{
    App, DEBOUNCE_TICKS, EditParamsState, EvalRequest, EvalScope, FieldEditor, Mode, Pane,
    PathAction,
};
use crate::event::Msg;
use crate::ui::palette::insert_letter;

/// Apply one message to the model. The only invariant callers rely on: a
/// panicking `update` crashes the process (no silent data loss), but a
/// returning `update` always leaves the model in a self-consistent state.
pub fn update(app: &mut App, msg: Msg) {
    match msg {
        Msg::NextPane => on_next_pane(app),
        Msg::ToggleHelp => app.show_help = !app.show_help,
        Msg::Dismiss => on_dismiss(app),
        Msg::Quit => on_quit(app),
        Msg::Tick => on_tick(app),

        Msg::RunAll => request_eval(app, EvalScope::All),
        Msg::RunToSelected => match app.selected {
            Some(sel) => request_eval(app, EvalScope::UpTo(sel)),
            None => app.status = "run to: no node selected".to_string(),
        },
        Msg::EvalDone {
            generation,
            report,
            error,
        } => on_eval_done(app, generation, report, error),

        // In EditParams mode Ctrl-S applies params rather than saving the file.
        Msg::Save => {
            if matches!(app.mode, Mode::EditParams(_)) {
                do_apply_params(app);
            } else {
                on_save(app);
            }
        }
        Msg::Open => on_open(app),

        // These are emitted by on_key routing but handled here so the match
        // is flat and all arms are visible in one place.
        Msg::InsertPending => {
            if app.focus == Pane::Canvas {
                app.mode = Mode::InsertPending;
                app.status = "insert: pick a module letter (Esc to cancel)".to_string();
            }
        }
        Msg::InsertKind(ch) => do_insert(app, ch),
        Msg::DeleteNode => do_delete_node(app),
        Msg::DeleteEdge => do_delete_edge(app),
        Msg::BeginConnect => do_begin_connect(app),
        Msg::ConfirmConnect => do_confirm_connect(app),
        Msg::StepFlow(fwd) => app.select_step(fwd),
        Msg::Lateral(right) => app.select_lateral(right),
        Msg::SelectBadge(n) => app.select_badge(n),

        Msg::PromptChar(ch) => {
            if let Mode::PromptPath { buf, .. } = &mut app.mode {
                buf.push(ch);
            }
        }
        Msg::PromptBackspace => {
            if let Mode::PromptPath { buf, .. } = &mut app.mode {
                buf.pop();
            }
        }
        Msg::PromptConfirm => do_confirm_prompt(app),

        Msg::Key(key) => on_key(app, key),
    }
}

// --- pane cycle (Tab) ----------------------------------------------------

/// Tab cycles panes in Normal modes; in EditParams it moves field focus.
fn on_next_pane(app: &mut App) {
    if matches!(app.mode, Mode::EditParams(_)) {
        shift_field_focus(app, true);
    } else {
        app.focus = app.focus.next();
    }
}

// --- mode dispatch -------------------------------------------------------

/// Route context-dependent keys based on the current mode and focused pane.
fn on_key(app: &mut App, key: KeyEvent) {
    match app.mode.clone() {
        Mode::InsertPending => {
            // Any character picks a module; anything else cancels.
            if let KeyCode::Char(ch) = key.code {
                update(app, Msg::InsertKind(ch));
            } else {
                app.mode = Mode::Normal;
                app.status.clear();
            }
        }
        Mode::PromptPath { .. } => {
            // Hand-rolled single-line text input.
            match key.code {
                KeyCode::Enter => update(app, Msg::PromptConfirm),
                KeyCode::Backspace => update(app, Msg::PromptBackspace),
                KeyCode::Char(ch) => update(app, Msg::PromptChar(ch)),
                _ => {}
            }
        }
        Mode::Connecting { .. } => {
            // Navigation moves the target; c/Enter commits; Esc cancels
            // (handled by Dismiss arm).
            match key.code {
                KeyCode::Char('j') | KeyCode::Down => update(app, Msg::StepFlow(true)),
                KeyCode::Char('k') | KeyCode::Up => update(app, Msg::StepFlow(false)),
                KeyCode::Char('h') => update(app, Msg::Lateral(false)),
                KeyCode::Char('l') => update(app, Msg::Lateral(true)),
                KeyCode::Char('c') | KeyCode::Enter => update(app, Msg::ConfirmConnect),
                KeyCode::Char(ch @ '1'..='9') => {
                    update(app, Msg::SelectBadge(ch as u64 - '0' as u64))
                }
                _ => {}
            }
        }
        Mode::QuitGuard => {
            // Only q confirms; everything else is ignored (Esc is Dismiss).
            if key.code == KeyCode::Char('q') {
                app.should_quit = true;
            }
        }
        Mode::EditParams(_) => on_key_edit_params(app, key),
        Mode::Normal => {
            // Run commands work from any pane, so they precede the canvas gate.
            match key.code {
                KeyCode::Char('r') => return update(app, Msg::RunAll),
                KeyCode::Char('R') => return update(app, Msg::RunToSelected),
                _ => {}
            }
            if app.focus != Pane::Canvas {
                return;
            }
            match key.code {
                KeyCode::Char('j') | KeyCode::Down => update(app, Msg::StepFlow(true)),
                KeyCode::Char('k') | KeyCode::Up => update(app, Msg::StepFlow(false)),
                KeyCode::Char('h') => update(app, Msg::Lateral(false)),
                KeyCode::Char('l') => update(app, Msg::Lateral(true)),
                KeyCode::Char('a') => update(app, Msg::InsertPending),
                KeyCode::Char('d') => update(app, Msg::DeleteNode),
                KeyCode::Char('x') => update(app, Msg::DeleteEdge),
                KeyCode::Char('c') => update(app, Msg::BeginConnect),
                KeyCode::Char(ch @ '1'..='9') => {
                    update(app, Msg::SelectBadge(ch as u64 - '0' as u64))
                }
                KeyCode::Char('q') => on_quit(app),
                KeyCode::Enter => {
                    if app.selected.is_some() {
                        open_params_overlay(app);
                    }
                }
                _ => {}
            }
        }
    }
}

fn on_dismiss(app: &mut App) {
    match &app.mode {
        Mode::Normal => app.show_help = false,
        Mode::EditParams(_) => {
            app.edit_state = None;
            app.mode = Mode::Normal;
        }
        _ => {
            app.mode = Mode::Normal;
            app.status.clear();
        }
    }
}

/// `q` / Ctrl-C: if dirty and not already in quit-guard, require confirmation.
fn on_quit(app: &mut App) {
    if app.dirty && !matches!(app.mode, Mode::QuitGuard) {
        app.mode = Mode::QuitGuard;
        app.status = "unsaved changes — q again to quit, Esc to stay".to_string();
    } else {
        app.should_quit = true;
    }
}

// --- live execution (M12) ------------------------------------------------

/// Arm the debounce timer after a graph edit. The run itself is spawned once
/// the countdown expires on a later `Tick`, so a burst of edits (typing into a
/// param field, several quick inserts) coalesces into one evaluation.
fn schedule_eval(app: &mut App) {
    app.eval.debounce = Some(DEBOUNCE_TICKS);
}

/// Request an eval *now*, bypassing the debounce (the manual `r`/`R` commands
/// and the fired debounce both land here). Bumps the generation so any run
/// already in flight is superseded, records the request for the event loop to
/// spawn, and marks the covered nodes loading so spinners appear at once.
fn request_eval(app: &mut App, scope: EvalScope) {
    app.eval.debounce = None;
    app.eval.generation += 1;
    app.eval.running = true;
    app.eval.loading = match scope {
        EvalScope::All => app.pipe.nodes.iter().map(|n| n.id).collect(),
        EvalScope::UpTo(target) => app.pipe.upstream_closure(&[target]),
    };
    app.eval.pending = Some(EvalRequest {
        generation: app.eval.generation,
        scope,
    });
    app.status = "evaluating…".to_string();
}

/// Once-per-tick timers: advance the spinner frame and step the debounce
/// countdown, firing a whole-pipe run when it hits zero.
fn on_tick(app: &mut App) {
    app.tick_count = app.tick_count.wrapping_add(1);
    match app.eval.debounce {
        Some(n) if n <= 1 => request_eval(app, EvalScope::All),
        Some(n) => app.eval.debounce = Some(n - 1),
        None => {}
    }
}

/// Fold a finished eval into the model. A result whose `generation` is not the
/// current one comes from a superseded run and is dropped — this is the guard
/// that stops a slow, stale eval from clobbering fresher output.
fn on_eval_done(app: &mut App, generation: u64, report: EvalReport, error: Option<String>) {
    if generation != app.eval.generation {
        return;
    }
    app.eval.running = false;
    app.eval.loading.clear();
    if let Some(err) = error {
        app.status = format!("eval failed: {}", err.lines().next().unwrap_or("error"));
        return;
    }
    // A scoped ("run to selected") run only reports the nodes it touched, so
    // merge over the existing statuses rather than replacing them; then prune
    // entries for nodes deleted since the run was spawned.
    let count = report.nodes.len();
    for (id, node_report) in report.nodes {
        app.statuses.insert(id, node_report);
    }
    app.statuses.retain(|id, _| app.pipe.node(*id).is_some());
    app.status = format!(
        "evaluated {count} node{}",
        if count == 1 { "" } else { "s" }
    );
}

// --- param-edit overlay --------------------------------------------------

/// Open the param-edit overlay for the currently-selected node.
fn open_params_overlay(app: &mut App) {
    let Some(node_id) = app.selected else { return };
    let state = EditParamsState::open(node_id, &app.registry, &app.pipe);
    app.edit_state = state;
    app.mode = Mode::EditParams(node_id);
    app.status.clear();
}

/// Move field focus by `delta` steps (wrapping), if the overlay is open.
fn shift_field_focus(app: &mut App, forward: bool) {
    let Some(state) = &mut app.edit_state else {
        return;
    };
    let n = state.editors.len();
    if n == 0 {
        return;
    }
    if forward {
        state.focused = (state.focused + 1) % n;
    } else {
        state.focused = (state.focused + n - 1) % n;
    }
}

/// Key handler for `Mode::EditParams`. Up/Down/Tab/Shift-Tab move field
/// focus; other keys are routed to the focused field's editor widget.
fn on_key_edit_params(app: &mut App, key: KeyEvent) {
    // Any key closes a no-field overlay.
    let has_fields = app
        .edit_state
        .as_ref()
        .is_some_and(|s| !s.editors.is_empty());
    if !has_fields {
        app.edit_state = None;
        app.mode = Mode::Normal;
        return;
    }

    match key.code {
        KeyCode::Down | KeyCode::Tab => shift_field_focus(app, true),
        KeyCode::Up | KeyCode::BackTab => shift_field_focus(app, false),
        _ => {
            let Some(state) = &mut app.edit_state else {
                return;
            };
            let focused = state.focused;
            let Some(editor) = state.editors.get_mut(focused) else {
                return;
            };
            match editor {
                FieldEditor::Text(ta) | FieldEditor::RuleList(ta) => {
                    let _ = ta.input(tui_textarea::Input::from(key));
                }
                FieldEditor::Bool(b) => {
                    if key.code == KeyCode::Char(' ') {
                        *b = !*b;
                    }
                }
                FieldEditor::Enum { variants, idx } => match key.code {
                    KeyCode::Char(' ') | KeyCode::Right => {
                        if !variants.is_empty() {
                            *idx = (*idx + 1) % variants.len();
                        }
                    }
                    KeyCode::Left => {
                        if !variants.is_empty() {
                            let n = variants.len();
                            *idx = (*idx + n - 1) % n;
                        }
                    }
                    _ => {}
                },
            }
        }
    }
}

/// Apply params from the overlay: validate per-field, then validate the pipe
/// transactionally (clone → mutate → validate → commit only if clean).
fn do_apply_params(app: &mut App) {
    let Mode::EditParams(node_id) = app.mode else {
        return;
    };

    // Collect schema and raw values without holding a borrow while we mutate.
    let (schema, raw_values) = {
        let Some(state) = &app.edit_state else { return };
        let schema = state.schema.clone();
        let raw_values: Vec<RawEditorValue> = state
            .editors
            .iter()
            .map(RawEditorValue::from_editor)
            .collect();
        (schema, raw_values)
    };

    let mut new_params = Params::new();
    let mut field_errors: Vec<Option<String>> = vec![None; raw_values.len()];
    let mut has_field_errors = false;

    for (i, (field, raw)) in schema.fields.iter().zip(raw_values.iter()).enumerate() {
        match (&field.kind, raw) {
            (FieldKind::Text | FieldKind::FieldName, RawEditorValue::Text(s)) => {
                if field.required && s.is_empty() {
                    field_errors[i] = Some(format!("`{}` is required", field.name));
                    has_field_errors = true;
                } else if !s.is_empty() {
                    new_params.set(field.name, s.clone());
                }
            }
            (FieldKind::Url, RawEditorValue::Text(s)) => {
                if field.required && s.is_empty() {
                    field_errors[i] = Some(format!("`{}` is required", field.name));
                    has_field_errors = true;
                } else if !s.is_empty() {
                    // URL must contain "://" or be a ${param} reference.
                    if !s.contains("://") && !s.contains("${") {
                        field_errors[i] =
                            Some("URL must contain '://' (or use ${param})".to_string());
                        has_field_errors = true;
                    }
                    new_params.set(field.name, s.clone());
                }
            }
            (FieldKind::Number, RawEditorValue::Text(s)) => {
                if s.is_empty() {
                    if field.required && field.default.is_none() {
                        field_errors[i] = Some(format!("`{}` is required", field.name));
                        has_field_errors = true;
                    }
                    // empty optional number → omit so schema default applies
                } else if s.starts_with("${") {
                    // ${param} reference — store as string; engine interpolates.
                    new_params.set(field.name, s.clone());
                } else {
                    match s.parse::<f64>() {
                        Ok(n) => {
                            new_params.set(
                                field.name,
                                serde_json::Number::from_f64(n)
                                    .map(serde_json::Value::Number)
                                    .unwrap_or(serde_json::Value::Null),
                            );
                        }
                        Err(_) => {
                            field_errors[i] = Some(format!("`{s}` is not a valid number"));
                            has_field_errors = true;
                        }
                    }
                }
            }
            (FieldKind::Bool, RawEditorValue::Bool(b)) => {
                new_params.set(field.name, *b);
            }
            (FieldKind::Enum(_), RawEditorValue::EnumIdx(idx)) => {
                // Look up the current variant name from the editor.
                let Some(state) = &app.edit_state else {
                    continue;
                };
                if let Some(FieldEditor::Enum { variants, .. }) = state.editors.get(i)
                    && let Some(&v) = variants.get(*idx)
                {
                    new_params.set(field.name, v);
                }
            }
            (FieldKind::RuleList, RawEditorValue::Lines(lines)) => {
                // Validate each non-blank line.
                let mut rule_errors: Vec<String> = Vec::new();
                for (line_idx, line) in lines.iter().enumerate() {
                    if field.name == "rules"
                        && let Err(e) = ripe_core::expr::compile(line)
                    {
                        rule_errors.push(format!("line {}: {e}", line_idx + 1));
                    }
                    // "ops" (transform) and others: accept any non-empty line.
                }
                if !rule_errors.is_empty() {
                    field_errors[i] = Some(rule_errors.join("; "));
                    has_field_errors = true;
                }
                if !lines.is_empty() {
                    let arr: Vec<serde_json::Value> = lines
                        .iter()
                        .map(|l| serde_json::Value::String(l.clone()))
                        .collect();
                    new_params.set(field.name, serde_json::Value::Array(arr));
                }
            }
            _ => {} // mismatched kind/raw — shouldn't happen; skip silently
        }
    }

    if has_field_errors {
        let state = app.edit_state.as_mut().unwrap();
        state.field_errors = field_errors;
        state.form_error = None;
        return;
    }

    // Transactional pipe validation: clone → mutate → validate → commit.
    let mut candidate = app.pipe.clone();
    if let Some(node) = candidate.node_mut(node_id) {
        node.params = new_params;
    }
    let errors = candidate.validate(&app.registry);
    if errors.is_empty() {
        app.pipe = candidate;
        app.dirty = true;
        app.edit_state = None;
        app.mode = Mode::Normal;
        app.status = format!("params saved for {node_id}");
        schedule_eval(app);
    } else {
        let state = app.edit_state.as_mut().unwrap();
        state.field_errors = field_errors;
        state.form_error = Some(errors[0].clone());
    }
}

/// A simple, lifetime-free snapshot of a `FieldEditor`'s current value,
/// extracted without holding a borrow on `edit_state`.
enum RawEditorValue {
    Text(String),
    Bool(bool),
    EnumIdx(usize),
    Lines(Vec<String>),
}

impl RawEditorValue {
    fn from_editor(editor: &FieldEditor) -> Self {
        match editor {
            FieldEditor::Text(ta) => {
                let first = ta.lines().first().cloned().unwrap_or_default();
                RawEditorValue::Text(first.trim().to_string())
            }
            FieldEditor::Bool(b) => RawEditorValue::Bool(*b),
            FieldEditor::Enum { idx, .. } => RawEditorValue::EnumIdx(*idx),
            FieldEditor::RuleList(ta) => {
                let lines: Vec<String> = ta
                    .lines()
                    .iter()
                    .map(|l| l.trim().to_string())
                    .filter(|l| !l.is_empty())
                    .collect();
                RawEditorValue::Lines(lines)
            }
        }
    }
}

// --- save / open ---------------------------------------------------------

fn on_save(app: &mut App) {
    match app.path.clone() {
        Some(path) => do_save(app, path),
        None => {
            app.mode = Mode::PromptPath {
                action: PathAction::Save,
                buf: String::new(),
            };
            app.status = "Save to: ".to_string();
        }
    }
}

fn do_save(app: &mut App, path: PathBuf) {
    match save_pipe(&app.pipe, &path) {
        Ok(()) => {
            app.path = Some(path);
            app.dirty = false;
            app.status = "saved".to_string();
        }
        Err(e) => {
            app.status = format!("save failed: {e}");
        }
    }
}

fn on_open(app: &mut App) {
    app.mode = Mode::PromptPath {
        action: PathAction::Open,
        buf: String::new(),
    };
    app.status = "Open: ".to_string();
}

fn do_confirm_prompt(app: &mut App) {
    let Mode::PromptPath {
        ref action,
        ref buf,
    } = app.mode.clone()
    else {
        return;
    };
    let path = PathBuf::from(buf);
    match action {
        PathAction::Save => {
            app.mode = Mode::Normal;
            do_save(app, path);
        }
        PathAction::Open => match load_pipe(&path, &app.registry) {
            Ok(loaded) => {
                for w in &loaded.warnings {
                    tracing::warn!("{w}");
                }
                app.pipe = loaded.pipe;
                app.path = Some(path);
                app.dirty = false;
                app.selected = app.pipe.topo_order().ok().and_then(|o| o.first().copied());
                app.statuses.clear();
                app.mode = Mode::Normal;
                app.status = "loaded".to_string();
                // A different graph entirely: drop the old memo cache, then
                // evaluate the newcomer.
                app.eval.reset_cache = true;
                schedule_eval(app);
            }
            Err(e) => {
                app.mode = Mode::Normal;
                app.status = format!("load failed: {e}");
            }
        },
    }
}

// --- insert --------------------------------------------------------------

/// Insert-mode: find the module kind for `ch` and add it to the pipe,
/// splicing it in after the selection when possible.
fn do_insert(app: &mut App, ch: char) {
    app.mode = Mode::Normal;

    // Resolve the letter to a kind.
    let Some(kind) = kind_for_letter(ch) else {
        app.status = format!("unknown insert letter '{ch}'");
        return;
    };

    // A letter must map to a registered kind; palette and this table are kept in
    // sync by construction (both live in this crate).
    let module = app
        .registry
        .get(kind)
        .expect("kind_for_letter only returns registered kinds");
    let can_receive = !module.inputs().is_empty();
    let can_emit = !module.outputs().is_empty();

    // Attempt splice if the selected node has outgoing edges AND the new
    // module has an input port (to wire after the selection) and an output
    // port (to forward the original targets).
    let do_splice = can_receive
        && can_emit
        && app
            .selected
            .is_some_and(|sel| app.pipe.edges.iter().any(|e| e.from.node == sel));

    if do_splice {
        let sel = app.selected.unwrap();
        // Collect the outgoing edges before we clone, so the clone is clean.
        let outgoing: Vec<_> = app
            .pipe
            .edges
            .iter()
            .filter(|e| e.from.node == sel)
            .cloned()
            .collect();

        let mut candidate = app.pipe.clone();
        let new_id = candidate.add_node(kind, Params::new());

        // Re-route: remove sel's outgoing edges, insert sel->new->old_targets.
        candidate.edges.retain(|e| e.from.node != sel);
        candidate.connect(sel, "out", new_id, "in");
        for edge in &outgoing {
            candidate.connect(new_id, "out", edge.to.node, &edge.to.port);
        }

        let errors = candidate.validate(&app.registry);
        if errors.is_empty() {
            app.pipe = candidate;
            app.selected = Some(new_id);
            app.dirty = true;
            app.status.clear();
            schedule_eval(app);
            return;
        }
        // Splice is type-invalid: fall back to adding unwired.
        app.status = format!("splice invalid ({}); added unwired", errors[0]);
    }

    // Unwired add (fallback or default when no outgoing edges).
    let new_id = app.pipe.add_node(kind, Params::new());
    app.selected = Some(new_id);
    app.dirty = true;
    schedule_eval(app);
}

// --- delete --------------------------------------------------------------

fn do_delete_node(app: &mut App) {
    let Some(id) = app.selected else {
        return;
    };
    let neighbor = sensible_neighbor(&app.pipe, id);
    app.pipe.remove_node(id);
    // Neighbor might itself have been removed (e.g. a self-edge pipe, which
    // validate() rejects, so should not happen in practice).
    app.selected = neighbor.filter(|&n| app.pipe.nodes.iter().any(|node| node.id == n));
    if app.selected.is_none() {
        app.selected = app.pipe.topo_order().ok().and_then(|o| o.first().copied());
    }
    // The node is gone; drop its stale status so nothing lingers on the canvas.
    app.statuses.remove(&id);
    app.dirty = true;
    app.status = format!("deleted {id}");
    schedule_eval(app);
}

fn do_delete_edge(app: &mut App) {
    let Some(id) = app.selected else { return };

    // Clone the edge so we drop the iterator borrow before mutating.
    let maybe_edge = app.pipe.edges_into(id).next().cloned();
    if let Some(edge) = maybe_edge {
        let desc = format!("{} → {}", edge.from.node, edge.to.node);
        app.pipe
            .remove_edge(edge.from.node, &edge.from.port, edge.to.node, &edge.to.port);
        app.dirty = true;
        app.status = format!("removed edge {desc}");
        schedule_eval(app);
    } else {
        app.status = format!("no edges into {id}");
    }
}

/// A node to select after deleting `id`: prefer a downstream neighbour, then
/// upstream, then the adjacent topo-order entry.
fn sensible_neighbor(pipe: &ripe_core::Pipe, id: ripe_core::NodeId) -> Option<ripe_core::NodeId> {
    if let Some(down) = pipe
        .edges
        .iter()
        .find(|e| e.from.node == id)
        .map(|e| e.to.node)
    {
        return Some(down);
    }
    if let Some(up) = pipe.edges_into(id).next().map(|e| e.from.node) {
        return Some(up);
    }
    if let Ok(order) = pipe.topo_order()
        && let Some(pos) = order.iter().position(|&n| n == id)
    {
        if pos + 1 < order.len() {
            return Some(order[pos + 1]);
        }
        if pos > 0 {
            return Some(order[pos - 1]);
        }
    }
    None
}

// --- connect -------------------------------------------------------------

fn do_begin_connect(app: &mut App) {
    let Some(from) = app.selected else {
        app.status = "select a source node first".to_string();
        return;
    };
    app.mode = Mode::Connecting { from };
    app.status = format!(
        "connect: navigate to target then press c or Enter (Esc to cancel) [source: {from}]"
    );
}

fn do_confirm_connect(app: &mut App) {
    let Mode::Connecting { from } = app.mode else {
        return;
    };
    let Some(to) = app.selected else {
        app.status = "no target selected".to_string();
        app.mode = Mode::Normal;
        return;
    };

    // Transactional: clone, attempt connect, validate, commit only if clean.
    let mut candidate = app.pipe.clone();
    candidate.connect(from, "out", to, "in");
    let errors = candidate.validate(&app.registry);
    if errors.is_empty() {
        app.pipe = candidate;
        app.dirty = true;
        app.status = format!("connected {from} → {to}");
        schedule_eval(app);
    } else {
        app.status = errors[0].clone();
    }
    app.mode = Mode::Normal;
}

// --- helpers -------------------------------------------------------------

/// The module kind whose insert letter (as shown in the palette) is `ch`.
/// Returns `None` for unmapped letters so `do_insert` can surface an error.
fn kind_for_letter(ch: char) -> Option<&'static str> {
    // All built-in kinds. Must stay in sync with `insert_letter` in palette.rs.
    const KINDS: &[&str] = &[
        "fetch_feed",
        "fetch_json",
        "fetch_csv",
        "filter",
        "regex",
        "transform",
        "sort",
        "union",
        "unique",
        "limit",
        "tail",
        "reverse",
        "output",
    ];
    KINDS
        .iter()
        .copied()
        .find(|&k| insert_letter(k) == Some(ch))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{EditParamsState, FieldEditor, Mode, Pane};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ripe_core::engine::{NodeReport, NodeStatus};
    use ripe_core::params::{FieldKind, FieldSpec, ParamSchema};
    use ripe_core::{NodeId, Params, Pipe, Registry};

    fn app() -> App {
        App::new(Registry::with_builtins())
    }

    fn canvas_app() -> (App, [NodeId; 3]) {
        let mut pipe = Pipe::new("nav");
        let a = pipe.add_node("fetch_feed", Params::new().with("url", "https://x"));
        let b = pipe.add_node("filter", Params::new());
        let c = pipe.add_node("output", Params::new());
        pipe.connect(a, "out", b, "in");
        pipe.connect(b, "out", c, "in");
        let mut app = App::with_pipe(Registry::with_builtins(), pipe, "nav.pipe".into());
        app.focus = Pane::Canvas;
        (app, [a, b, c])
    }

    fn key_msg(code: KeyCode) -> Msg {
        Msg::Key(KeyEvent::new(code, KeyModifiers::empty()))
    }

    // --- existing M8/M9 tests (preserved) --------------------------------

    #[test]
    fn tab_cycles_focus_through_all_panes_and_wraps() {
        let mut app = app();
        assert_eq!(app.focus, Pane::Palette);
        update(&mut app, Msg::NextPane);
        assert_eq!(app.focus, Pane::Canvas);
        update(&mut app, Msg::NextPane);
        assert_eq!(app.focus, Pane::Preview);
        update(&mut app, Msg::NextPane);
        assert_eq!(app.focus, Pane::Palette);
    }

    #[test]
    fn quit_ctrl_c_always_quits() {
        let mut app = app();
        assert!(!app.should_quit);
        // Ctrl-C (Msg::Quit) bypasses the dirty guard.
        update(&mut app, Msg::Quit);
        assert!(app.should_quit);
    }

    #[test]
    fn help_toggles_and_esc_closes_it() {
        let mut app = app();
        assert!(!app.show_help);
        update(&mut app, Msg::ToggleHelp);
        assert!(app.show_help);
        update(&mut app, Msg::ToggleHelp);
        assert!(!app.show_help);
        update(&mut app, Msg::ToggleHelp);
        update(&mut app, Msg::Dismiss);
        assert!(!app.show_help);
    }

    #[test]
    fn unbound_keys_do_not_quit() {
        let mut app = app();
        let ev = KeyEvent::new(KeyCode::Char('z'), KeyModifiers::empty());
        update(&mut app, Msg::Key(ev));
        update(&mut app, Msg::Tick);
        assert!(!app.should_quit);
        assert!(!app.show_help);
    }

    #[test]
    fn jk_walk_the_selection_and_clamp_at_the_ends() {
        let (mut app, [a, b, c]) = canvas_app();
        assert_eq!(app.selected, Some(a));

        update(&mut app, key_msg(KeyCode::Char('j')));
        assert_eq!(app.selected, Some(b));
        update(&mut app, key_msg(KeyCode::Down));
        assert_eq!(app.selected, Some(c));
        update(&mut app, key_msg(KeyCode::Char('j')));
        assert_eq!(app.selected, Some(c));

        update(&mut app, key_msg(KeyCode::Char('k')));
        assert_eq!(app.selected, Some(b));
        update(&mut app, key_msg(KeyCode::Up));
        assert_eq!(app.selected, Some(a));
        update(&mut app, key_msg(KeyCode::Char('k')));
        assert_eq!(app.selected, Some(a));
    }

    #[test]
    fn motion_keys_are_inert_outside_the_canvas() {
        let (mut app, [a, _, _]) = canvas_app();
        app.focus = Pane::Palette;
        update(&mut app, key_msg(KeyCode::Char('j')));
        assert_eq!(
            app.selected,
            Some(a),
            "j does nothing when palette holds focus"
        );
    }

    // --- M10 insert tests ------------------------------------------------

    #[test]
    fn insert_adds_node_unwired_when_no_selection() {
        let mut app = app();
        app.focus = Pane::Canvas;
        assert_eq!(app.pipe.nodes.len(), 0);
        update(&mut app, key_msg(KeyCode::Char('a')));
        assert_eq!(app.mode, Mode::InsertPending);
        update(&mut app, key_msg(KeyCode::Char('f'))); // fetch_feed
        assert_eq!(app.pipe.nodes.len(), 1);
        assert_eq!(app.pipe.nodes[0].kind, "fetch_feed");
        assert!(app.dirty);
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn insert_pending_esc_cancels() {
        let mut app = app();
        app.focus = Pane::Canvas;
        update(&mut app, key_msg(KeyCode::Char('a')));
        assert_eq!(app.mode, Mode::InsertPending);
        update(&mut app, Msg::Dismiss);
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.pipe.nodes.len(), 0);
    }

    #[test]
    fn insert_splices_after_selected_node_with_outgoing_edges() {
        let (mut app, [a, b, _c]) = canvas_app();
        // Select a (which has outgoing edge to b): inserting filter splices between a and b.
        app.selected = Some(a);
        update(&mut app, key_msg(KeyCode::Char('a')));
        update(&mut app, key_msg(KeyCode::Char('t'))); // filter

        // The splice should wire a -> new_filter -> b.
        assert_eq!(app.pipe.nodes.len(), 4);
        let new_id = app.selected.unwrap();
        assert_ne!(new_id, a);
        assert_ne!(new_id, b);
        assert_eq!(app.pipe.node(new_id).unwrap().kind, "filter");
        // a feeds new_id
        assert!(
            app.pipe
                .edges
                .iter()
                .any(|e| e.from.node == a && e.to.node == new_id)
        );
        // new_id feeds b
        assert!(
            app.pipe
                .edges
                .iter()
                .any(|e| e.from.node == new_id && e.to.node == b)
        );
        // Original a->b edge is gone
        assert!(
            !app.pipe
                .edges
                .iter()
                .any(|e| e.from.node == a && e.to.node == b)
        );
        assert!(app.dirty);
    }

    #[test]
    fn insert_falls_back_to_unwired_on_type_invalid_splice() {
        // fetch_feed has no input port, so splicing it after another node is invalid.
        let (mut app, [a, _, _]) = canvas_app();
        app.selected = Some(a); // a has outgoing edges

        update(&mut app, key_msg(KeyCode::Char('a')));
        update(&mut app, key_msg(KeyCode::Char('f'))); // fetch_feed — no input port

        // Node added but unwired (splice of a source into a middle position is invalid).
        assert_eq!(app.pipe.nodes.len(), 4);
        let new_id = app.selected.unwrap();
        assert_eq!(app.pipe.node(new_id).unwrap().kind, "fetch_feed");
        // No edge from a to new_id (splice rejected).
        assert!(
            !app.pipe
                .edges
                .iter()
                .any(|e| e.from.node == a && e.to.node == new_id)
        );
        assert!(
            !app.status.is_empty(),
            "status should describe the fallback"
        );
        assert!(app.dirty);
    }

    #[test]
    fn insert_unknown_letter_gives_error_status() {
        let mut app = app();
        app.focus = Pane::Canvas;
        update(&mut app, key_msg(KeyCode::Char('a')));
        update(&mut app, key_msg(KeyCode::Char('z'))); // not assigned
        assert_eq!(app.pipe.nodes.len(), 0);
        assert!(!app.status.is_empty());
        assert_eq!(app.mode, Mode::Normal);
    }

    // --- M10 delete tests ------------------------------------------------

    #[test]
    fn delete_node_removes_it_and_its_edges() {
        let (mut app, [a, b, c]) = canvas_app();
        app.selected = Some(b);
        update(&mut app, key_msg(KeyCode::Char('d')));
        assert_eq!(app.pipe.nodes.len(), 2);
        assert!(app.pipe.node(b).is_none());
        // Both edges touching b are gone.
        assert!(
            !app.pipe
                .edges
                .iter()
                .any(|e| e.from.node == b || e.to.node == b)
        );
        // Selection moves to a neighbour.
        assert!(
            app.selected == Some(a) || app.selected == Some(c),
            "selection should be a or c, got {:?}",
            app.selected
        );
        assert!(app.dirty);
    }

    #[test]
    fn delete_edge_removes_first_incoming_edge() {
        let (mut app, [a, b, _c]) = canvas_app();
        app.selected = Some(b);
        let edge_count = app.pipe.edges.len();
        update(&mut app, key_msg(KeyCode::Char('x')));
        assert_eq!(app.pipe.edges.len(), edge_count - 1);
        // The edge from a into b is gone.
        assert!(
            !app.pipe
                .edges
                .iter()
                .any(|e| e.from.node == a && e.to.node == b)
        );
        assert!(app.dirty);
        assert!(!app.status.is_empty());
    }

    #[test]
    fn delete_edge_is_noop_with_status_when_no_incoming_edges() {
        let (mut app, [a, _b, _c]) = canvas_app();
        app.selected = Some(a); // a is a source: no incoming edges
        let edge_count = app.pipe.edges.len();
        update(&mut app, key_msg(KeyCode::Char('x')));
        assert_eq!(app.pipe.edges.len(), edge_count);
        assert!(!app.status.is_empty());
    }

    // --- M10 connect tests -----------------------------------------------

    #[test]
    fn connect_creates_edge_between_two_nodes() {
        let mut pipe = Pipe::new("conn");
        let a = pipe.add_node("fetch_feed", Params::new().with("url", "https://x"));
        let b = pipe.add_node("filter", Params::new());
        let mut app = App::with_pipe(Registry::with_builtins(), pipe, "p.pipe".into());
        app.focus = Pane::Canvas;
        app.selected = Some(a);

        update(&mut app, key_msg(KeyCode::Char('c'))); // begin connect
        assert!(matches!(app.mode, Mode::Connecting { from } if from == a));

        app.selected = Some(b);
        update(&mut app, key_msg(KeyCode::Char('c'))); // confirm
        assert_eq!(app.mode, Mode::Normal);
        assert!(
            app.pipe
                .edges
                .iter()
                .any(|e| e.from.node == a && e.to.node == b)
        );
        assert!(app.dirty);
    }

    #[test]
    fn connect_rejects_cycle_with_message_and_pipe_unchanged() {
        let (mut app, [a, _, c]) = canvas_app();
        // Try to connect c (downstream) back into a (upstream) — cycle.
        app.selected = Some(c);
        update(&mut app, key_msg(KeyCode::Char('c')));
        app.selected = Some(a);
        update(&mut app, key_msg(KeyCode::Char('c')));
        assert_eq!(app.mode, Mode::Normal);
        // Pipe should be unchanged (no new edge).
        assert_eq!(app.pipe.edges.len(), 2);
        assert!(!app.status.is_empty(), "error message expected");
    }

    #[test]
    fn connect_rejects_double_fan_in_on_non_variadic_port() {
        let mut pipe = Pipe::new("fanin");
        let a = pipe.add_node("fetch_feed", Params::new().with("url", "https://a"));
        let b = pipe.add_node("fetch_feed", Params::new().with("url", "https://b"));
        let f = pipe.add_node("filter", Params::new());
        pipe.connect(a, "out", f, "in"); // filter.in already occupied
        let mut app = App::with_pipe(Registry::with_builtins(), pipe, "p.pipe".into());
        app.focus = Pane::Canvas;
        let edge_count = app.pipe.edges.len();

        // Try to connect b -> f as well (f.in is non-variadic).
        app.selected = Some(b);
        update(&mut app, key_msg(KeyCode::Char('c')));
        app.selected = Some(f);
        update(&mut app, key_msg(KeyCode::Char('c')));
        assert_eq!(app.pipe.edges.len(), edge_count, "pipe must be unchanged");
        assert!(!app.status.is_empty(), "error message expected");
    }

    // --- M10 navigation tests --------------------------------------------

    #[test]
    fn badge_jump_selects_correct_node() {
        let (mut app, [a, b, c]) = canvas_app();
        // NodeIds are 1, 2, 3 (Pipe assigns sequentially from 1).
        app.selected = Some(a);
        update(&mut app, key_msg(KeyCode::Char('3')));
        assert_eq!(app.selected, Some(c));
        update(&mut app, key_msg(KeyCode::Char('2')));
        assert_eq!(app.selected, Some(b));
        update(&mut app, key_msg(KeyCode::Char('1')));
        assert_eq!(app.selected, Some(a));
    }

    #[test]
    fn badge_jump_is_noop_for_nonexistent_badge() {
        let (mut app, [a, _, _]) = canvas_app();
        app.selected = Some(a);
        update(&mut app, key_msg(KeyCode::Char('9')));
        assert_eq!(app.selected, Some(a)); // no node with id 9
    }

    #[test]
    fn h_l_cross_branches() {
        // Two branches: fetch_feed col 0, fetch_feed col 1, both into union.
        let mut pipe = Pipe::new("branches");
        let left = pipe.add_node("fetch_feed", Params::new().with("url", "https://a"));
        let right = pipe.add_node("fetch_feed", Params::new().with("url", "https://b"));
        let union = pipe.add_node("union", Params::new());
        pipe.connect(left, "out", union, "in");
        pipe.connect(right, "out", union, "in");

        let mut app = App::with_pipe(Registry::with_builtins(), pipe, "b.pipe".into());
        app.focus = Pane::Canvas;
        app.selected = Some(left);

        update(&mut app, key_msg(KeyCode::Char('l')));
        assert_eq!(app.selected, Some(right));
        update(&mut app, key_msg(KeyCode::Char('h')));
        assert_eq!(app.selected, Some(left));
    }

    // --- M10 dirty / quit guard tests ------------------------------------

    #[test]
    fn dirty_is_set_by_edits_and_cleared_by_save() {
        let mut app = app();
        app.focus = Pane::Canvas;
        assert!(!app.dirty);
        update(&mut app, key_msg(KeyCode::Char('a')));
        update(&mut app, key_msg(KeyCode::Char('f')));
        assert!(app.dirty);

        let tmp = std::env::temp_dir().join(format!("ripe-m10-dirty-{}.pipe", std::process::id()));
        app.path = Some(tmp.clone());
        update(&mut app, Msg::Save);
        assert!(!app.dirty);
        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn quit_guard_requires_second_q_with_dirty_pipe() {
        let mut app = app();
        app.focus = Pane::Canvas;
        // Make it dirty.
        update(&mut app, key_msg(KeyCode::Char('a')));
        update(&mut app, key_msg(KeyCode::Char('f')));
        assert!(app.dirty);

        // First q: enters quit guard.
        update(&mut app, key_msg(KeyCode::Char('q')));
        assert_eq!(app.mode, Mode::QuitGuard);
        assert!(!app.should_quit);

        // Esc: back to Normal.
        update(&mut app, Msg::Dismiss);
        assert_eq!(app.mode, Mode::Normal);
        assert!(!app.should_quit);

        // Enter quit guard again.
        update(&mut app, key_msg(KeyCode::Char('q')));
        // Second q: actually quits.
        update(&mut app, key_msg(KeyCode::Char('q')));
        assert!(app.should_quit);
    }

    #[test]
    fn quit_skips_guard_on_clean_pipe() {
        let mut app = app();
        app.focus = Pane::Canvas;
        update(&mut app, key_msg(KeyCode::Char('q')));
        // No dirty flag: goes straight to should_quit.
        assert!(app.should_quit);
    }

    // --- M11 param overlay tests -----------------------------------------

    #[test]
    fn enter_on_canvas_opens_param_overlay_for_selected_node() {
        let (mut app, [_a, b, _c]) = canvas_app();
        app.selected = Some(b); // b is a filter node
        update(&mut app, key_msg(KeyCode::Enter));
        assert!(
            matches!(app.mode, Mode::EditParams(id) if id == b),
            "mode should be EditParams(b), got {:?}",
            app.mode
        );
        assert!(app.edit_state.is_some(), "edit_state must be populated");
    }

    #[test]
    fn enter_without_selection_does_not_open_overlay() {
        let mut app = app();
        app.focus = Pane::Canvas;
        app.selected = None;
        update(&mut app, key_msg(KeyCode::Enter));
        assert_eq!(app.mode, Mode::Normal);
        assert!(app.edit_state.is_none());
    }

    #[test]
    fn esc_from_overlay_discards_edits_and_returns_to_normal() {
        let (mut app, [_a, b, _c]) = canvas_app();
        app.selected = Some(b);
        // Open overlay.
        update(&mut app, key_msg(KeyCode::Enter));
        assert!(matches!(app.mode, Mode::EditParams(_)));
        // Type something (goes into rules field TextArea).
        update(&mut app, key_msg(KeyCode::Char('x')));
        // Esc discards.
        update(&mut app, Msg::Dismiss);
        assert_eq!(app.mode, Mode::Normal);
        assert!(app.edit_state.is_none());
        // Params are unchanged.
        assert_eq!(app.pipe.node(b).unwrap().params.get("rules"), None);
        assert!(!app.dirty, "Esc must not mark dirty");
    }

    #[test]
    fn ctrl_s_in_overlay_applies_rules_sets_params_and_dirty() {
        let (mut app, [_a, b, _c]) = canvas_app();
        app.selected = Some(b); // filter node; field 0 is "rules" (RuleList)
        update(&mut app, key_msg(KeyCode::Enter));
        assert!(matches!(app.mode, Mode::EditParams(_)));

        // Type "score > 100" into the focused rules TextArea.
        for ch in "score > 100".chars() {
            update(&mut app, key_msg(KeyCode::Char(ch)));
        }

        // Apply with Ctrl-s.
        update(&mut app, Msg::Save);
        assert_eq!(
            app.mode,
            Mode::Normal,
            "overlay should close on successful apply"
        );
        assert!(app.edit_state.is_none());
        assert!(app.dirty);

        let rules = app.pipe.node(b).unwrap().params.get("rules").unwrap();
        let arr = rules.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0].as_str().unwrap(), "score > 100");
    }

    #[test]
    fn bad_rule_blocks_apply_and_shows_inline_error() {
        let (mut app, [_a, b, _c]) = canvas_app();
        app.selected = Some(b);
        update(&mut app, key_msg(KeyCode::Enter));

        // Type an invalid rule.
        for ch in "score >> 1".chars() {
            update(&mut app, key_msg(KeyCode::Char(ch)));
        }

        update(&mut app, Msg::Save);

        // Overlay must remain open.
        assert!(
            matches!(app.mode, Mode::EditParams(_)),
            "overlay must stay open after bad rule"
        );
        // A field error must be set.
        let state = app.edit_state.as_ref().unwrap();
        assert!(
            state.field_errors.iter().any(|e| e.is_some()),
            "field_errors must be non-empty"
        );
        // Pipe must be unchanged.
        assert_eq!(app.pipe.node(b).unwrap().params.get("rules"), None);
        assert!(!app.dirty);
    }

    #[test]
    fn number_field_stores_valid_number_as_json_number() {
        // Use the "limit" module which has a required Number field "n".
        let mut pipe = Pipe::new("lim");
        let limit_id = pipe.add_node("limit", Params::new());
        let mut app = App::with_pipe(Registry::with_builtins(), pipe, "l.pipe".into());
        app.focus = Pane::Canvas;
        app.selected = Some(limit_id);

        update(&mut app, key_msg(KeyCode::Enter));
        // Type "25".
        for ch in "25".chars() {
            update(&mut app, key_msg(KeyCode::Char(ch)));
        }
        update(&mut app, Msg::Save);

        assert_eq!(app.mode, Mode::Normal);
        let n = app.pipe.node(limit_id).unwrap().params.get("n").unwrap();
        assert_eq!(n.as_f64(), Some(25.0));
    }

    #[test]
    fn number_field_invalid_text_shows_inline_error() {
        let mut pipe = Pipe::new("lim");
        let limit_id = pipe.add_node("limit", Params::new());
        let mut app = App::with_pipe(Registry::with_builtins(), pipe, "l.pipe".into());
        app.focus = Pane::Canvas;
        app.selected = Some(limit_id);

        update(&mut app, key_msg(KeyCode::Enter));
        for ch in "abc".chars() {
            update(&mut app, key_msg(KeyCode::Char(ch)));
        }
        update(&mut app, Msg::Save);

        // Overlay stays open.
        assert!(matches!(app.mode, Mode::EditParams(_)));
        let state = app.edit_state.as_ref().unwrap();
        assert!(state.field_errors.iter().any(|e| e.is_some()));
        assert!(!app.dirty);
    }

    #[test]
    fn number_field_param_ref_stored_as_string() {
        let mut pipe = Pipe::new("lim");
        let limit_id = pipe.add_node("limit", Params::new());
        // Add a pipe-level param "n" so the ref is declared.
        pipe.params.push(ripe_core::PipeParam {
            name: "n".to_string(),
            kind: ripe_core::PipeParamKind::Number,
            default: None,
        });
        let mut app = App::with_pipe(Registry::with_builtins(), pipe, "l.pipe".into());
        app.focus = Pane::Canvas;
        app.selected = Some(limit_id);

        update(&mut app, key_msg(KeyCode::Enter));
        for ch in "${n}".chars() {
            update(&mut app, key_msg(KeyCode::Char(ch)));
        }
        update(&mut app, Msg::Save);

        assert_eq!(
            app.mode,
            Mode::Normal,
            "apply should succeed: ${{n}} is a valid declared ref"
        );
        let val = app.pipe.node(limit_id).unwrap().params.get("n").unwrap();
        assert_eq!(val.as_str(), Some("${n}"), "param ref stored as string");
    }

    #[test]
    fn bool_field_toggles_on_space() {
        // Directly build edit state with a Bool field (no built-in has Bool,
        // so we test the editor in isolation without going through a real node).
        let mut app = app();

        let schema = ParamSchema::new(vec![FieldSpec::optional(
            "active",
            "Active",
            FieldKind::Bool,
        )]);
        let state = EditParamsState {
            schema,
            focused: 0,
            editors: vec![FieldEditor::Bool(false)],
            field_errors: vec![None],
            form_error: None,
        };
        app.edit_state = Some(state);
        // NodeId(99) is a sentinel — we're only testing key routing, not apply.
        app.mode = Mode::EditParams(NodeId(99));

        update(&mut app, key_msg(KeyCode::Char(' ')));

        let st = app.edit_state.as_ref().unwrap();
        assert!(
            matches!(st.editors[0], FieldEditor::Bool(true)),
            "Space must flip bool to true"
        );

        update(&mut app, key_msg(KeyCode::Char(' ')));
        let st = app.edit_state.as_ref().unwrap();
        assert!(
            matches!(st.editors[0], FieldEditor::Bool(false)),
            "second Space flips back to false"
        );
    }

    #[test]
    fn enum_field_cycles_on_space_and_left_right() {
        let mut app = app();

        let schema = ParamSchema::new(vec![FieldSpec::optional(
            "mode",
            "Mode",
            FieldKind::Enum(&["permit", "block", "log"]),
        )]);
        let state = EditParamsState {
            schema,
            focused: 0,
            editors: vec![FieldEditor::Enum {
                variants: &["permit", "block", "log"],
                idx: 0,
            }],
            field_errors: vec![None],
            form_error: None,
        };
        app.edit_state = Some(state);
        app.mode = Mode::EditParams(NodeId(99));

        // Space → advance to "block"
        update(&mut app, key_msg(KeyCode::Char(' ')));
        let st = app.edit_state.as_ref().unwrap();
        assert!(matches!(st.editors[0], FieldEditor::Enum { idx: 1, .. }));

        // Right → advance to "log"
        update(&mut app, key_msg(KeyCode::Right));
        let st = app.edit_state.as_ref().unwrap();
        assert!(matches!(st.editors[0], FieldEditor::Enum { idx: 2, .. }));

        // Right wraps → back to "permit"
        update(&mut app, key_msg(KeyCode::Right));
        let st = app.edit_state.as_ref().unwrap();
        assert!(matches!(st.editors[0], FieldEditor::Enum { idx: 0, .. }));

        // Left wraps → "log"
        update(&mut app, key_msg(KeyCode::Left));
        let st = app.edit_state.as_ref().unwrap();
        assert!(matches!(st.editors[0], FieldEditor::Enum { idx: 2, .. }));
    }

    #[test]
    fn undeclared_param_ref_in_text_field_rejected_at_apply() {
        // A node with a URL field (fetch_feed).
        let mut pipe = Pipe::new("test");
        let fetch_id = pipe.add_node("fetch_feed", Params::new());
        let mut app = App::with_pipe(Registry::with_builtins(), pipe, "test.pipe".into());
        app.focus = Pane::Canvas;
        app.selected = Some(fetch_id);

        update(&mut app, key_msg(KeyCode::Enter));
        // Type "${nope}" — contains "${" so URL validation passes, but
        // "nope" is undeclared, so pipe.validate() will catch it.
        for ch in "${nope}".chars() {
            update(&mut app, key_msg(KeyCode::Char(ch)));
        }
        update(&mut app, Msg::Save);

        // Overlay must stay open with a form error.
        assert!(
            matches!(app.mode, Mode::EditParams(_)),
            "overlay must stay open when pipe validation fails"
        );
        let state = app.edit_state.as_ref().unwrap();
        assert!(
            state.form_error.is_some(),
            "form_error must be set for undeclared ref"
        );
        // Pipe is unchanged.
        assert_eq!(app.pipe.node(fetch_id).unwrap().params.get("url"), None);
        assert!(!app.dirty);
    }

    #[test]
    fn tab_in_overlay_moves_field_focus_not_pane() {
        let (mut app, [_a, b, _c]) = canvas_app();
        app.selected = Some(b); // filter: 3 fields
        update(&mut app, key_msg(KeyCode::Enter));
        let initial_pane = app.focus;

        // Tab should move field focus, not switch pane.
        update(&mut app, Msg::NextPane);
        assert_eq!(app.focus, initial_pane, "pane must not change in overlay");
        let st = app.edit_state.as_ref().unwrap();
        assert_eq!(st.focused, 1, "field focus must advance to 1");

        // Tab again.
        update(&mut app, Msg::NextPane);
        let st = app.edit_state.as_ref().unwrap();
        assert_eq!(st.focused, 2, "field focus must advance to 2");

        // Tab wraps.
        update(&mut app, Msg::NextPane);
        let st = app.edit_state.as_ref().unwrap();
        assert_eq!(st.focused, 0, "field focus must wrap to 0");
    }

    // --- M10 acceptance test (mockup pipe from keyboard) -----------------

    /// Build the mockup pipe entirely through update(), save, reload, and
    /// compare nodes + edges. This is the "Done when" bar for M10.
    ///
    /// Mockup (docs/mockup.png):
    ///   fetch_feed(HN) → filter → regex → filter → union ← fetch_feed(reddit)
    ///                                                  ↓
    ///                                               sort → output
    ///
    /// Building strategy: insert each node, then wire with explicit connect
    /// presses. Splice is exercised at the end (sort → output inserted after
    /// sort already has an outgoing edge). The unit test
    /// `insert_splices_after_selected_node_with_outgoing_edges` covers the
    /// transactional splice path in full.
    #[test]
    fn acceptance_build_mockup_save_load_roundtrip() {
        let registry = Registry::with_builtins();
        let mut app = App::new(registry);
        app.focus = Pane::Canvas;

        // Helper: press a character key in the canvas.
        let press = |app: &mut App, ch: char| {
            update(
                app,
                Msg::Key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::empty())),
            );
        };
        // Helper: connect src to tgt via `c` press.
        let connect = |app: &mut App, src: NodeId, tgt: NodeId| {
            app.selected = Some(src);
            update(
                app,
                Msg::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::empty())),
            );
            app.selected = Some(tgt);
            update(
                app,
                Msg::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::empty())),
            );
        };

        // 1. Insert fetch_feed (HN).
        press(&mut app, 'a');
        press(&mut app, 'f'); // fetch_feed
        let hn = app.selected.expect("hn selected");
        assert_eq!(app.pipe.node(hn).unwrap().kind, "fetch_feed");

        // 2. Insert filter1 (unwired; hn has no outgoing edges yet).
        press(&mut app, 'a');
        press(&mut app, 't'); // filter
        let filter1 = app.selected.expect("filter1 selected");

        // 3. Wire hn → filter1.
        connect(&mut app, hn, filter1);
        assert!(
            app.pipe
                .edges
                .iter()
                .any(|e| e.from.node == hn && e.to.node == filter1)
        );

        // 4. Insert regex (unwired; filter1 has no outgoing edges).
        press(&mut app, 'a');
        press(&mut app, 'r'); // regex
        let regex = app.selected.expect("regex selected");

        // 5. Wire filter1 → regex.
        connect(&mut app, filter1, regex);

        // 6. Insert filter2 (unwired; regex has no outgoing edges).
        press(&mut app, 'a');
        press(&mut app, 't'); // filter
        let filter2 = app.selected.expect("filter2 selected");

        // 7. Wire regex → filter2.
        connect(&mut app, regex, filter2);

        // 8. Insert union (unwired).
        press(&mut app, 'a');
        press(&mut app, 'u'); // union
        let union = app.selected.expect("union selected");

        // 9. Wire filter2 → union.
        connect(&mut app, filter2, union);

        // 10. Insert second fetch_feed (reddit) — source, always added unwired.
        press(&mut app, 'a');
        press(&mut app, 'f'); // fetch_feed
        let reddit = app.selected.expect("reddit selected");

        // 11. Wire reddit → union.
        connect(&mut app, reddit, union);

        // 12. Insert sort (unwired; union has no outgoing edges yet).
        press(&mut app, 'a');
        press(&mut app, 's'); // sort
        let sort = app.selected.expect("sort selected");

        // 13. Wire union → sort.
        connect(&mut app, union, sort);

        // 14. Insert output after sort — sort now has an outgoing edge (to nothing
        //     yet at this point), so we insert it and then splice to exercise the
        //     splice path. First wire sort → output via explicit connect.
        press(&mut app, 'a');
        press(&mut app, 'o'); // output
        let output = app.selected.expect("output selected");

        // 15. Wire sort → output.
        connect(&mut app, sort, output);

        // Now exercise splice: insert a `limit` node between sort and output.
        // sort → output currently exists; selecting sort and inserting limit will
        // splice: sort → limit → output.
        app.selected = Some(sort);
        press(&mut app, 'a');
        press(&mut app, 'l'); // limit
        let limit = app.selected.expect("limit selected");
        // Verify splice wired correctly: sort → limit and limit → output.
        assert!(
            app.pipe
                .edges
                .iter()
                .any(|e| e.from.node == sort && e.to.node == limit),
            "splice should wire sort → limit"
        );
        assert!(
            app.pipe
                .edges
                .iter()
                .any(|e| e.from.node == limit && e.to.node == output),
            "splice should wire limit → output"
        );
        // The old sort → output edge should be gone.
        assert!(
            !app.pipe
                .edges
                .iter()
                .any(|e| e.from.node == sort && e.to.node == output),
            "splice should remove old sort → output edge"
        );

        // Pipe should be valid (9 nodes: hn, filter1, regex, filter2, reddit, union, sort, limit, output).
        let errors = app.pipe.validate(&app.registry);
        assert!(errors.is_empty(), "built pipe has errors: {errors:?}");
        assert_eq!(app.pipe.nodes.len(), 9);
        assert!(app.dirty);

        // Save to a temp file.
        let tmp =
            std::env::temp_dir().join(format!("ripe-m10-acceptance-{}.pipe", std::process::id()));
        app.path = Some(tmp.clone());
        update(&mut app, Msg::Save);
        assert!(!app.dirty, "save must clear dirty flag");

        // Load back and compare nodes/edges exactly.
        let loaded = ripe_core::persist::load_pipe(&tmp, &Registry::with_builtins())
            .expect("load must succeed");
        assert_eq!(loaded.pipe.nodes.len(), app.pipe.nodes.len());
        assert_eq!(loaded.pipe.edges.len(), app.pipe.edges.len());
        for (orig, reloaded) in app.pipe.nodes.iter().zip(loaded.pipe.nodes.iter()) {
            assert_eq!(orig.kind, reloaded.kind);
            assert_eq!(orig.id, reloaded.id);
        }
        assert_eq!(loaded.pipe.edges, app.pipe.edges);

        std::fs::remove_file(&tmp).ok();
    }

    // --- M11 acceptance test (overlay edits change module output) --------

    #[tokio::test]
    async fn acceptance_overlay_filter_params_change_output() {
        use ripe_core::fetch::FetchClient;
        use ripe_core::item::{Item, PortValue};
        use ripe_core::module::{EvalCtx, Ins};

        let registry = Registry::with_builtins();
        let mut pipe = Pipe::new("test");
        let filter_id = pipe.add_node("filter", Params::new());
        let mut app = App::with_pipe(registry, pipe, "test.pipe".into());
        app.focus = Pane::Canvas;
        app.selected = Some(filter_id);

        // Open overlay.
        update(
            &mut app,
            Msg::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty())),
        );
        assert!(
            matches!(app.mode, Mode::EditParams(_)),
            "overlay should open"
        );

        // Type "score > 100" into the rules field.
        for ch in "score > 100".chars() {
            update(
                &mut app,
                Msg::Key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::empty())),
            );
        }

        // Apply.
        update(&mut app, Msg::Save);
        assert_eq!(app.mode, Mode::Normal, "overlay closes after apply");
        assert!(app.dirty);

        // Verify params set.
        let params = app.pipe.node(filter_id).unwrap().params.clone();
        let rules = params.get("rules").expect("rules must be set");
        let arr = rules.as_array().expect("rules must be array");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0].as_str().unwrap(), "score > 100");

        // Eval filter directly with hand-built inputs.
        let reg = Registry::with_builtins();
        let filter_module = reg.get("filter").unwrap();
        let ctx = EvalCtx::new(FetchClient::default());

        fn make_item(score: serde_json::Value) -> Item {
            Item(
                serde_json::json!({"score": score})
                    .as_object()
                    .unwrap()
                    .clone(),
            )
        }

        let mut ins = Ins::default();
        ins.push(
            "in",
            PortValue::Items(vec![
                make_item(serde_json::json!(50)),
                make_item(serde_json::json!(150)),
                make_item(serde_json::json!("200")),
            ]),
        );

        let outs = filter_module.eval(&ctx, ins, &params).await.unwrap();
        let items = match outs.get("out").unwrap() {
            PortValue::Items(items) => items,
            _ => panic!("expected Items on `out`"),
        };

        // Scores 150 and "200" (string-coerced to 200.0) are > 100; 50 is not.
        assert_eq!(items.len(), 2, "exactly two items should pass score > 100");
        let scores: Vec<f64> = items
            .iter()
            .filter_map(|item| item.get("score").and_then(ripe_core::expr::value_f64))
            .collect();
        assert!(scores.contains(&150.0));
        assert!(scores.contains(&200.0));
    }

    // --- M12 live-execution tests ----------------------------------------

    /// An all-`Ok` report over `ids`, standing in for a finished eval.
    fn ok_report(ids: &[NodeId]) -> EvalReport {
        let mut report = EvalReport::default();
        for &id in ids {
            report.nodes.insert(
                id,
                NodeReport {
                    status: NodeStatus::Ok,
                    error: None,
                    duration: std::time::Duration::ZERO,
                    item_count: Some(1),
                },
            );
        }
        report
    }

    #[test]
    fn a_burst_of_edits_coalesces_into_one_debounced_run() {
        let (mut app, _) = canvas_app();
        // Two edits before any tick fires.
        update(&mut app, key_msg(KeyCode::Char('a')));
        update(&mut app, key_msg(KeyCode::Char('f'))); // insert fetch_feed
        update(&mut app, key_msg(KeyCode::Char('a')));
        update(&mut app, key_msg(KeyCode::Char('t'))); // insert filter

        assert_eq!(app.eval.debounce, Some(DEBOUNCE_TICKS));
        assert!(app.eval.pending.is_none(), "no run while typing settles");
        assert_eq!(app.eval.generation, 0);

        // Drain the debounce; the run fires exactly once.
        update(&mut app, Msg::Tick); // Some(2) -> Some(1)
        assert!(app.eval.pending.is_none());
        assert_eq!(app.eval.generation, 0);
        update(&mut app, Msg::Tick); // Some(1) -> fire
        let req = app.eval.pending.expect("debounce fires a request");
        assert_eq!(req.scope, EvalScope::All);
        assert_eq!(app.eval.generation, 1);

        // Idle ticks never start a second run.
        update(&mut app, Msg::Tick);
        assert_eq!(app.eval.generation, 1, "settled edits run only once");
    }

    #[test]
    fn each_edit_re_arms_the_debounce() {
        let (mut app, [a, _b, _c]) = canvas_app();
        app.selected = Some(a);
        update(&mut app, key_msg(KeyCode::Char('a')));
        update(&mut app, key_msg(KeyCode::Char('t'))); // edit -> debounce armed
        update(&mut app, Msg::Tick); // Some(2) -> Some(1)
        assert_eq!(app.eval.debounce, Some(1));

        // A fresh edit resets the countdown to the top.
        update(&mut app, key_msg(KeyCode::Char('a')));
        update(&mut app, key_msg(KeyCode::Char('s'))); // sort
        assert_eq!(app.eval.debounce, Some(DEBOUNCE_TICKS));
        assert_eq!(app.eval.generation, 0, "still no run");
    }

    #[test]
    fn run_all_marks_every_node_loading_and_requests_all() {
        let (mut app, [a, b, c]) = canvas_app();
        update(&mut app, key_msg(KeyCode::Char('r')));
        let req = app.eval.pending.expect("run requested");
        assert_eq!(req.scope, EvalScope::All);
        assert_eq!(req.generation, 1);
        assert_eq!(app.eval.loading, [a, b, c].into_iter().collect());
        assert!(app.eval.running);
    }

    #[test]
    fn run_to_selected_loads_only_the_ancestor_closure() {
        let (mut app, [a, b, c]) = canvas_app(); // a -> b -> c
        app.selected = Some(b);
        update(&mut app, key_msg(KeyCode::Char('R')));
        let req = app.eval.pending.expect("run requested");
        assert_eq!(req.scope, EvalScope::UpTo(b));
        // b and its upstream a, but never the downstream c.
        assert_eq!(app.eval.loading, [a, b].into_iter().collect());
        assert!(!app.eval.loading.contains(&c));
    }

    #[test]
    fn run_to_selected_without_selection_is_a_noop() {
        let mut app = app();
        app.focus = Pane::Canvas;
        app.selected = None;
        update(&mut app, key_msg(KeyCode::Char('R')));
        assert!(app.eval.pending.is_none());
        assert_eq!(app.eval.generation, 0);
    }

    #[test]
    fn a_stale_eval_result_is_dropped() {
        let (mut app, [a, b, c]) = canvas_app();
        update(&mut app, Msg::RunAll); // generation 1
        assert_eq!(app.eval.generation, 1);
        // A second run supersedes the first before it returns.
        update(&mut app, Msg::RunAll); // generation 2
        assert_eq!(app.eval.generation, 2);

        // The late gen-1 result must not touch the model.
        update(
            &mut app,
            Msg::EvalDone {
                generation: 1,
                report: ok_report(&[a, b, c]),
                error: None,
            },
        );
        assert!(
            app.statuses.is_empty(),
            "stale result must not populate statuses"
        );
        assert!(app.eval.running, "still waiting on the current run");
        assert_eq!(app.eval.loading.len(), 3, "loading survives a stale result");

        // The current gen-2 result lands and is applied.
        update(
            &mut app,
            Msg::EvalDone {
                generation: 2,
                report: ok_report(&[a, b, c]),
                error: None,
            },
        );
        assert_eq!(app.statuses.len(), 3);
        assert!(!app.eval.running);
        assert!(app.eval.loading.is_empty());
    }

    #[test]
    fn eval_done_applies_report_and_prunes_deleted_nodes() {
        let (mut app, [a, b, _c]) = canvas_app();
        update(&mut app, Msg::RunAll);

        // The report references a node deleted while the run was in flight.
        let mut report = ok_report(&[a, b]);
        report.nodes.insert(
            NodeId(999),
            NodeReport {
                status: NodeStatus::Ok,
                error: None,
                duration: std::time::Duration::ZERO,
                item_count: Some(0),
            },
        );
        update(
            &mut app,
            Msg::EvalDone {
                generation: 1,
                report,
                error: None,
            },
        );
        assert!(app.statuses.contains_key(&a));
        assert!(app.statuses.contains_key(&b));
        assert!(
            !app.statuses.contains_key(&NodeId(999)),
            "a report entry for a since-deleted node is pruned"
        );
        assert!(app.eval.loading.is_empty());
    }

    #[test]
    fn eval_done_error_sets_status_and_clears_loading() {
        let (mut app, _) = canvas_app();
        update(&mut app, Msg::RunAll);
        update(
            &mut app,
            Msg::EvalDone {
                generation: 1,
                report: EvalReport::default(),
                error: Some("pipe contains a cycle (through node #2)".to_string()),
            },
        );
        assert!(!app.eval.running);
        assert!(app.eval.loading.is_empty());
        assert!(app.status.contains("cycle"), "status was: {}", app.status);
        assert!(app.statuses.is_empty());
    }

    #[test]
    fn r_inside_the_param_overlay_types_rather_than_runs() {
        let (mut app, [_a, b, _c]) = canvas_app();
        app.selected = Some(b); // filter node
        update(&mut app, key_msg(KeyCode::Enter)); // open overlay
        assert!(matches!(app.mode, Mode::EditParams(_)));

        update(&mut app, key_msg(KeyCode::Char('r')));
        // No run: `r` was consumed by the focused text field.
        assert!(app.eval.pending.is_none());
        assert_eq!(app.eval.generation, 0);
    }

    #[test]
    fn opening_a_pipe_requests_a_cache_reset_and_a_run() {
        // Save a pipe, then load it and confirm the reset+run is queued.
        let (mut app, _) = canvas_app();
        let tmp = std::env::temp_dir().join(format!("ripe-m12-open-{}.pipe", std::process::id()));
        save_pipe(&app.pipe, &tmp).unwrap();

        // Drive the Open prompt.
        update(&mut app, Msg::Open);
        for ch in tmp.to_string_lossy().chars() {
            update(&mut app, key_msg(KeyCode::Char(ch)));
        }
        update(&mut app, key_msg(KeyCode::Enter));

        assert!(app.eval.reset_cache, "load must drop the stale memo cache");
        assert_eq!(
            app.eval.debounce,
            Some(DEBOUNCE_TICKS),
            "load schedules a run"
        );
        std::fs::remove_file(&tmp).ok();
    }
}
