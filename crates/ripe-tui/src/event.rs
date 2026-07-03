//! Input translation: crossterm events become semantic [`Msg`]s so `update`
//! never touches raw key codes. Keeping the mapping here means the keymap
//! lives in one place and stays trivially testable.

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// A thing that happened, framed in the app's own vocabulary.
///
/// [`Msg::Key`] carries keys we recognized structurally but have no binding
/// for yet — the panes claim them as editing lands in later milestones.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Msg {
    /// Move focus to the next pane.
    NextPane,
    /// Toggle the help overlay.
    ToggleHelp,
    /// Dismiss a transient overlay (currently just help).
    Dismiss,
    /// Tear down and exit.
    Quit,
    /// A key with no binding yet.
    Key(KeyEvent),
    /// The periodic timer fired (spinners, future async polling cadence).
    Tick,
}

/// Translate a terminal event into a [`Msg`], or `None` when it carries
/// nothing the app acts on. Resize is a `None`: the loop redraws every
/// iteration regardless, so the new size is picked up on the next frame.
pub fn from_event(event: Event) -> Option<Msg> {
    match event {
        // Windows reports both press and release; act on presses only so a
        // binding never fires twice.
        Event::Key(key) if key.kind == KeyEventKind::Press => Some(from_key(key)),
        _ => None,
    }
}

/// Map a single key press to a [`Msg`]. `q` quits and `Ctrl-C` quits from
/// anywhere; the leader-based insert keys (PLAN.md) arrive in M10.
pub fn from_key(key: KeyEvent) -> Msg {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return Msg::Quit;
    }
    match key.code {
        KeyCode::Tab => Msg::NextPane,
        KeyCode::Char('?') => Msg::ToggleHelp,
        KeyCode::Char('q') => Msg::Quit,
        KeyCode::Esc => Msg::Dismiss,
        _ => Msg::Key(key),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::empty())
    }

    #[test]
    fn bindings_map_to_their_messages() {
        assert_eq!(from_key(key(KeyCode::Tab)), Msg::NextPane);
        assert_eq!(from_key(key(KeyCode::Char('?'))), Msg::ToggleHelp);
        assert_eq!(from_key(key(KeyCode::Char('q'))), Msg::Quit);
        assert_eq!(from_key(key(KeyCode::Esc)), Msg::Dismiss);
    }

    #[test]
    fn ctrl_c_quits() {
        let ev = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(from_key(ev), Msg::Quit);
    }

    #[test]
    fn unbound_key_falls_through_to_key() {
        let ev = key(KeyCode::Char('x'));
        assert_eq!(from_key(ev), Msg::Key(ev));
    }

    #[test]
    fn key_release_is_ignored() {
        let mut ev = key(KeyCode::Char('q'));
        ev.kind = KeyEventKind::Release;
        assert_eq!(from_event(Event::Key(ev)), None);
    }
}
