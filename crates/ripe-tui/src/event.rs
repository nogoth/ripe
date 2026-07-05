//! Input translation: crossterm events become semantic [`Msg`]s so `update`
//! never touches raw key codes. Keeping the mapping here means the keymap
//! lives in one place and stays trivially testable.

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use ripe_core::EvalReport;

/// A thing that happened, framed in the app's own vocabulary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Msg {
    // --- global ---------------------------------------------------------
    /// Move focus to the next pane.
    NextPane,
    /// Toggle the help overlay.
    ToggleHelp,
    /// Dismiss / cancel the current modal or overlay (Esc).
    Dismiss,
    /// Tear down and exit.
    Quit,
    /// The periodic timer fired (advances the debounce countdown + spinner).
    Tick,

    // --- live execution (M12) -------------------------------------------
    /// Run the whole pipe now (`r`).
    RunAll,
    /// Run only the selected node and its upstreams now (`R`).
    RunToSelected,
    /// An async eval finished. `generation` lets `update` discard the result
    /// if a newer run has since superseded it; `error` is set only when the
    /// run failed structurally (an empty `report` accompanies it).
    EvalDone {
        generation: u64,
        report: EvalReport,
        error: Option<String>,
    },

    // --- canvas editing (M10) -------------------------------------------
    /// Save to the current path, or prompt if no path is set yet.
    Save,
    /// Prompt for a path and load.
    Open,
    /// Enter insert-pending mode (`a`).
    InsertPending,
    /// Complete an insert with the module kind whose letter was pressed.
    InsertKind(char),
    /// Delete the selected node (and its edges).
    DeleteNode,
    /// Delete the first edge *into* the selected node.
    DeleteEdge,
    /// Mark the selected node as the pending connect source.
    BeginConnect,
    /// Attempt to wire the pending source to the currently-selected node.
    ConfirmConnect,
    /// Move selection one step downstream (`j`) or upstream (`k`).
    StepFlow(bool),
    /// Move selection left/right within the same layout row (`h`/`l`).
    Lateral(bool),
    /// Jump to the node whose badge number equals `n` (keys `1`–`9`).
    SelectBadge(u64),

    // --- path prompt (M10) ----------------------------------------------
    /// Append a character while typing a file path.
    PromptChar(char),
    /// Delete the last character in the path prompt.
    PromptBackspace,
    /// Confirm the typed path.
    PromptConfirm,

    /// A key with no binding in the current context.
    Key(KeyEvent),
}

/// Translate a terminal event into a [`Msg`], or `None` when it carries
/// nothing the app acts on. Resize is `None`: the loop redraws every
/// iteration so the new size is picked up on the next frame.
pub fn from_event(event: Event) -> Option<Msg> {
    match event {
        Event::Key(key) if key.kind == KeyEventKind::Press => Some(from_key(key)),
        _ => None,
    }
}

/// Map a single key press to a [`Msg`]. Most context-dependent keys (insert
/// letters, delete, connect) fall through as [`Msg::Key`] so `update` can
/// dispatch them based on the current mode and focused pane.
pub fn from_key(key: KeyEvent) -> Msg {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return match key.code {
            KeyCode::Char('c') => Msg::Quit,
            KeyCode::Char('s') => Msg::Save,
            KeyCode::Char('o') => Msg::Open,
            _ => Msg::Key(key),
        };
    }
    match key.code {
        KeyCode::Tab => Msg::NextPane,
        KeyCode::Char('?') => Msg::ToggleHelp,
        KeyCode::Esc => Msg::Dismiss,
        // All other keys are context-dependent; update() reads the mode.
        _ => Msg::Key(key),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::empty())
    }

    fn ctrl(ch: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL)
    }

    #[test]
    fn global_bindings_map_to_their_messages() {
        assert_eq!(from_key(key(KeyCode::Tab)), Msg::NextPane);
        assert_eq!(from_key(key(KeyCode::Char('?'))), Msg::ToggleHelp);
        assert_eq!(from_key(key(KeyCode::Esc)), Msg::Dismiss);
        assert_eq!(from_key(ctrl('c')), Msg::Quit);
        assert_eq!(from_key(ctrl('s')), Msg::Save);
        assert_eq!(from_key(ctrl('o')), Msg::Open);
    }

    #[test]
    fn q_and_editing_keys_fall_through_to_key() {
        // q, a, d, x, c, j, k, h, l, r, R, 1-9 are all context-dependent:
        // update() reads the mode, so e.g. `r` runs in Normal but types in a
        // param field.
        for ch in [
            'q', 'a', 'd', 'x', 'c', 'j', 'k', 'h', 'l', 'r', 'R', '1', '5', '9',
        ] {
            let ev = key(KeyCode::Char(ch));
            assert_eq!(
                from_key(ev),
                Msg::Key(ev),
                "char '{ch}' should fall through"
            );
        }
    }

    #[test]
    fn key_release_is_ignored() {
        let mut ev = key(KeyCode::Char('q'));
        ev.kind = KeyEventKind::Release;
        assert_eq!(from_event(Event::Key(ev)), None);
    }
}
