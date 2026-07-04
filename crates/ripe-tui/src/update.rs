//! The state transition: `update(&mut App, Msg)`. Pure and synchronous —
//! it only mutates the model. Async work (eval) is dispatched here starting
//! in M12; M10 adds all the editing interactions that make ripe actually
//! usable from the keyboard.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent};

use ripe_core::Params;
use ripe_core::persist::{load_pipe, save_pipe};

use crate::app::{App, Mode, Pane, PathAction};
use crate::event::Msg;
use crate::ui::palette::insert_letter;

/// Apply one message to the model. The only invariant callers rely on: a
/// panicking `update` crashes the process (no silent data loss), but a
/// returning `update` always leaves the model in a self-consistent state.
pub fn update(app: &mut App, msg: Msg) {
    match msg {
        Msg::NextPane => app.focus = app.focus.next(),
        Msg::ToggleHelp => app.show_help = !app.show_help,
        Msg::Dismiss => on_dismiss(app),
        Msg::Quit => on_quit(app),
        Msg::Tick => {}

        Msg::Save => on_save(app),
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
        Mode::Normal => {
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
                _ => {}
            }
        }
    }
}

fn on_dismiss(app: &mut App) {
    match &app.mode {
        Mode::Normal => app.show_help = false,
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
            return;
        }
        // Splice is type-invalid: fall back to adding unwired.
        app.status = format!("splice invalid ({}); added unwired", errors[0]);
    }

    // Unwired add (fallback or default when no outgoing edges).
    let new_id = app.pipe.add_node(kind, Params::new());
    app.selected = Some(new_id);
    app.dirty = true;
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
    app.dirty = true;
    app.status = format!("deleted {id}");
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
    use crate::app::{Mode, Pane};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
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
}
