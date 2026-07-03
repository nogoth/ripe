//! The state transition: `update(&mut App, Msg)`. Pure and synchronous —
//! it only mutates the model. Async work (eval) is dispatched here and its
//! results arrive as further `Msg`s starting in M12; M8 has none.

use crate::app::App;
use crate::event::Msg;

/// Apply one message to the model.
pub fn update(app: &mut App, msg: Msg) {
    match msg {
        Msg::NextPane => app.focus = app.focus.next(),
        Msg::ToggleHelp => app.show_help = !app.show_help,
        Msg::Dismiss => app.show_help = false,
        Msg::Quit => app.should_quit = true,
        // Unbound keys and the tick change nothing yet.
        Msg::Key(_) | Msg::Tick => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Pane;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ripe_core::Registry;

    fn app() -> App {
        App::new(Registry::with_builtins())
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
}
