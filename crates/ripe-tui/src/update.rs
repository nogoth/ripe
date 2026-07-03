//! The state transition: `update(&mut App, Msg)`. Pure and synchronous —
//! it only mutates the model. Async work (eval) is dispatched here and its
//! results arrive as further `Msg`s starting in M12; M8 has none.

use crossterm::event::{KeyCode, KeyEvent};

use crate::app::{App, Pane};
use crate::event::Msg;

/// Apply one message to the model.
pub fn update(app: &mut App, msg: Msg) {
    match msg {
        Msg::NextPane => app.focus = app.focus.next(),
        Msg::ToggleHelp => app.show_help = !app.show_help,
        Msg::Dismiss => app.show_help = false,
        Msg::Quit => app.should_quit = true,
        Msg::Key(key) => on_key(app, key),
        // The tick changes nothing yet.
        Msg::Tick => {}
    }
}

/// Keys with no global binding are offered to the focused pane. The canvas
/// claims the vim/arrow motions to walk the selection; the canvas view
/// auto-scrolls to keep it visible, so nothing here touches `scroll`.
fn on_key(app: &mut App, key: KeyEvent) {
    if app.focus == Pane::Canvas {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => app.select_step(true),
            KeyCode::Char('k') | KeyCode::Up => app.select_step(false),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Pane;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ripe_core::{Params, Pipe, Registry};

    fn app() -> App {
        App::new(Registry::with_builtins())
    }

    fn key_msg(code: KeyCode) -> Msg {
        Msg::Key(KeyEvent::new(code, KeyModifiers::empty()))
    }

    /// A three-node linear pipe with the canvas focused.
    fn canvas_app() -> (App, [ripe_core::NodeId; 3]) {
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
    fn quit_sets_the_flag() {
        let mut app = app();
        assert!(!app.should_quit);
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
        // Esc closes an open overlay and is a no-op otherwise.
        update(&mut app, Msg::ToggleHelp);
        update(&mut app, Msg::Dismiss);
        assert!(!app.show_help);
    }

    #[test]
    fn unbound_keys_do_not_quit() {
        let mut app = app();
        let ev = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::empty());
        update(&mut app, Msg::Key(ev));
        update(&mut app, Msg::Tick);
        assert!(!app.should_quit);
        assert!(!app.show_help);
    }

    #[test]
    fn jk_walk_the_selection_and_clamp_at_the_ends() {
        let (mut app, [a, b, c]) = canvas_app();
        assert_eq!(app.selected, Some(a), "selection starts at the first node");

        update(&mut app, key_msg(KeyCode::Char('j')));
        assert_eq!(app.selected, Some(b));
        update(&mut app, key_msg(KeyCode::Down));
        assert_eq!(app.selected, Some(c));
        // Already at the last node: j clamps, never wraps.
        update(&mut app, key_msg(KeyCode::Char('j')));
        assert_eq!(app.selected, Some(c));

        update(&mut app, key_msg(KeyCode::Char('k')));
        assert_eq!(app.selected, Some(b));
        update(&mut app, key_msg(KeyCode::Up));
        assert_eq!(app.selected, Some(a));
        // Already at the first node: k clamps.
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
            "j does nothing when the palette holds focus"
        );
    }
}
