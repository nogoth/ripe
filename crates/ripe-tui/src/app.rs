//! The App model: everything the view renders and the update fn mutates.
//!
//! M8 is a skeleton, so the model is deliberately thin — a loaded pipe, which
//! pane has focus, and the two global toggles (help overlay, quit). Selection,
//! viewport, and per-node eval state arrive with the milestones that need them.

use std::path::PathBuf;

use ripe_core::{Pipe, Registry};

/// Which pane holds keyboard focus. `Tab` walks them in this order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Palette,
    Canvas,
    Preview,
}

impl Pane {
    /// The next pane in the focus cycle: Palette -> Canvas -> Preview -> Palette.
    pub fn next(self) -> Self {
        match self {
            Pane::Palette => Pane::Canvas,
            Pane::Canvas => Pane::Preview,
            Pane::Preview => Pane::Palette,
        }
    }
}

/// The editor state. Owns the registry so the palette can group the real
/// module list without threading it through the view.
pub struct App {
    /// Module catalog, shared by palette, loader, and (later) engine.
    pub registry: Registry,
    /// The pipe under edit. A fresh session starts on an empty, unsaved pipe.
    pub pipe: Pipe,
    /// Where the pipe was loaded from / will save to; `None` until first save.
    pub path: Option<PathBuf>,
    /// Whether the pipe has unsaved edits. Wired to real editing in M10; M8
    /// has no mutations, so it never becomes true here.
    pub dirty: bool,
    /// The pane keyboard input is directed at.
    pub focus: Pane,
    /// Whether the help overlay is drawn over the layout.
    pub show_help: bool,
    /// Set once the event loop should tear down and restore the terminal.
    pub should_quit: bool,
}

impl App {
    /// A new session on an empty, unsaved pipe.
    pub fn new(registry: Registry) -> Self {
        Self::from_parts(registry, Pipe::new("untitled"), None)
    }

    /// A session editing `pipe`, remembering where it came from.
    pub fn with_pipe(registry: Registry, pipe: Pipe, path: PathBuf) -> Self {
        Self::from_parts(registry, pipe, Some(path))
    }

    fn from_parts(registry: Registry, pipe: Pipe, path: Option<PathBuf>) -> Self {
        Self {
            registry,
            pipe,
            path,
            dirty: false,
            focus: Pane::Palette,
            show_help: false,
            should_quit: false,
        }
    }

    /// The bare filename shown in the top bar; "untitled" before the first save.
    pub fn file_label(&self) -> String {
        self.path
            .as_deref()
            .and_then(std::path::Path::file_name)
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "untitled".to_string())
    }

    /// Node count, shown in the top bar. Derived from the pipe rather than
    /// cached so it cannot drift out of sync with edits.
    pub fn node_count(&self) -> usize {
        self.pipe.nodes.len()
    }
}
