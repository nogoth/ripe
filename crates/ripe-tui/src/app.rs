//! The App model: everything the view renders and the update fn mutates.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

use ripe_core::engine::NodeReport;
use ripe_core::params::{FieldKind, ParamSchema};
use ripe_core::preview::Preview;
use ripe_core::{NodeId, Pipe, Registry};

use crate::ui::layout::Layout;

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

/// What to do when the path prompt is confirmed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathAction {
    /// Save the current pipe to the prompted path.
    Save,
    /// Load a pipe from the prompted path.
    Open,
}

/// What the editor is doing right now. Every non-Normal mode renders a hint
/// and captures the full key stream until resolved or cancelled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    /// Normal editing — all keybindings active.
    Normal,
    /// `a` was pressed; the next key picks a module kind by its palette letter.
    InsertPending,
    /// `c` was pressed on `from`; navigate to a target then press `c`/Enter.
    Connecting { from: NodeId },
    /// Typing a file path in the status line (one-line hand-rolled input).
    PromptPath { action: PathAction, buf: String },
    /// `q` was pressed with unsaved edits; press `q` again to confirm quit.
    QuitGuard,
    /// The param-edit overlay is open for the given node. The widget state
    /// lives in `App::edit_state`; keeping it separate lets `Mode` stay
    /// `Clone + PartialEq` without threading `TextArea`'s lifetime through.
    EditParams(NodeId),
}

// ---- param-edit overlay state -------------------------------------------

/// Per-field editing widget. Text/Number/RuleList fields use tui-textarea for
/// cursor-capable input; Bool and Enum use simple toggle/cycle state.
pub enum FieldEditor {
    /// Single-line input — used for Text, Url, FieldName, and Number fields.
    Text(tui_textarea::TextArea<'static>),
    /// Boolean toggle — Space flips the value.
    Bool(bool),
    /// Enum cycle — Space / Left / Right walks the variants in order.
    Enum {
        variants: &'static [&'static str],
        idx: usize,
    },
    /// Multi-line rule list — one entry per line.
    RuleList(tui_textarea::TextArea<'static>),
}

/// The state kept while the param-edit overlay is open.
/// Held in `App::edit_state` rather than inlined in `Mode`.
pub struct EditParamsState {
    /// Schema of the node under edit.
    pub schema: ParamSchema,
    /// Index of the field currently focused (0-based, wraps).
    pub focused: usize,
    /// One editor per schema field, in schema order.
    pub editors: Vec<FieldEditor>,
    /// Per-field inline error from the last failed apply (`None` = clean).
    pub field_errors: Vec<Option<String>>,
    /// Whole-form error (e.g., pipe structural validation rejected a `${ref}`).
    pub form_error: Option<String>,
}

impl EditParamsState {
    /// Initialise overlay state from the node's current params. Returns `None`
    /// if the node or its registered module cannot be found.
    pub fn open(node_id: NodeId, registry: &Registry, pipe: &Pipe) -> Option<Self> {
        let node = pipe.node(node_id)?;
        let module = registry.get(&node.kind)?;
        let schema = module.param_schema();
        let params = &node.params;

        let editors: Vec<FieldEditor> = schema
            .fields
            .iter()
            .map(|field| match &field.kind {
                FieldKind::Text | FieldKind::Url | FieldKind::FieldName | FieldKind::Number => {
                    let content = match params.get(field.name) {
                        Some(serde_json::Value::String(s)) => s.clone(),
                        Some(v) => v.to_string(),
                        None => String::new(),
                    };
                    let mut ta = tui_textarea::TextArea::default();
                    if !content.is_empty() {
                        ta.insert_str(&content);
                    }
                    FieldEditor::Text(ta)
                }
                FieldKind::Bool => {
                    let val = params.get_bool(field.name).unwrap_or_else(|| {
                        field
                            .default
                            .as_ref()
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false)
                    });
                    FieldEditor::Bool(val)
                }
                FieldKind::Enum(variants) => {
                    let current = params
                        .get_str(field.name)
                        .or_else(|| field.default.as_ref().and_then(|v| v.as_str()));
                    let idx = current
                        .and_then(|s| variants.iter().position(|&v| v == s))
                        .unwrap_or(0);
                    FieldEditor::Enum { variants, idx }
                }
                FieldKind::RuleList => {
                    let rules: Vec<String> = params
                        .get(field.name)
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| v.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default();
                    let ta = if rules.is_empty() {
                        tui_textarea::TextArea::default()
                    } else {
                        tui_textarea::TextArea::new(rules)
                    };
                    FieldEditor::RuleList(ta)
                }
            })
            .collect();

        let n = editors.len();
        Some(EditParamsState {
            schema,
            focused: 0,
            editors,
            field_errors: vec![None; n],
            form_error: None,
        })
    }
}

// ---- live-eval wiring (M12) ---------------------------------------------

/// How much of the graph one eval run covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvalScope {
    /// The whole pipe. Memoization keeps this cheap: only nodes whose memo key
    /// changed since the last run actually re-evaluate.
    All,
    /// Only `target` and its transitive upstreams ("run to selected").
    UpTo(NodeId),
}

/// A request `update` hands to the event loop, which owns the async runtime,
/// the shared cache, and the HTTP client. The loop spawns the eval and tags
/// its `EvalDone` message with this `generation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EvalRequest {
    pub generation: u64,
    pub scope: EvalScope,
}

/// Live-eval bookkeeping. `update` mutates this synchronously; the event loop
/// reads `pending`/`reset_cache` and performs the actual spawning, so the
/// model stays pure and testable while I/O lives in `main`.
#[derive(Debug, Default)]
pub struct EvalState {
    /// Monotonic run id. Bumped on every request; an `EvalDone` whose
    /// generation is older than this is stale and dropped (supersede).
    pub generation: u64,
    /// A request the event loop has not yet spawned. Set by `update`, taken by
    /// the loop.
    pub pending: Option<EvalRequest>,
    /// Ticks left before a debounced edit becomes a `pending` request. `None`
    /// means no debounce is armed. Each edit re-arms it, so a burst of edits
    /// coalesces into a single run once typing settles.
    pub debounce: Option<u8>,
    /// Ask the loop to drop the memo cache before the next run (set on load,
    /// when the whole graph is replaced).
    pub reset_cache: bool,
    /// Nodes whose result the current run is still computing — drawn with a
    /// spinner on the canvas until the run completes.
    pub loading: BTreeSet<NodeId>,
    /// Whether a run is in flight (drives the status line).
    pub running: bool,
}

/// Ticks (at the event loop's ~250ms cadence) a burst of edits must settle
/// before a debounced re-eval fires. Two ticks ≈ up to half a second.
pub const DEBOUNCE_TICKS: u8 = 2;

// ---- preview panel (M13) ------------------------------------------------

/// Which tab the preview panel shows. `[` / `]` walk them when the preview
/// pane holds focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewTab {
    /// Channel-level metadata (title, link, description, counts).
    Feed,
    /// A card per item (title, domain, age, snippet).
    Items,
    /// The serialized feed, reusing `format.rs`.
    Raw,
}

impl PreviewTab {
    /// Zero-based position in the FEED / ITEMS / RAW tab bar.
    pub fn index(self) -> usize {
        match self {
            PreviewTab::Feed => 0,
            PreviewTab::Items => 1,
            PreviewTab::Raw => 2,
        }
    }

    /// The next tab (`]`), wrapping FEED -> ITEMS -> RAW -> FEED.
    pub fn next(self) -> Self {
        match self {
            PreviewTab::Feed => PreviewTab::Items,
            PreviewTab::Items => PreviewTab::Raw,
            PreviewTab::Raw => PreviewTab::Feed,
        }
    }

    /// The previous tab (`[`), wrapping the other way.
    pub fn prev(self) -> Self {
        match self {
            PreviewTab::Feed => PreviewTab::Raw,
            PreviewTab::Items => PreviewTab::Feed,
            PreviewTab::Raw => PreviewTab::Items,
        }
    }
}

/// Everything the preview panel needs. The `snapshot` is precomputed off the
/// render thread (see [`Preview`]) and swapped in wholesale on eval; the view
/// never re-serializes or reads a clock.
pub struct PreviewState {
    /// The visible tab.
    pub tab: PreviewTab,
    /// When on, each re-eval refreshes the panel; when off, the last snapshot
    /// is frozen (eval still runs — canvas statuses keep updating).
    pub auto_refresh: bool,
    /// Vertical scroll offset into the current tab's body, clamped by the view.
    pub scroll: u16,
    /// The last output stream rendered, or `None` before the first successful
    /// run (or while an error is shown).
    pub snapshot: Option<Preview>,
    /// Set when the latest covered run could not produce an output stream:
    /// the failing node + message, or a "no Output node" hint. Shown instead
    /// of stale output.
    pub error: Option<String>,
}

impl Default for PreviewState {
    fn default() -> Self {
        Self {
            tab: PreviewTab::Items,
            auto_refresh: true,
            scroll: 0,
            snapshot: None,
            error: None,
        }
    }
}

/// The editor state. Owns the registry so the palette can group the real
/// module list without threading it through the view.
pub struct App {
    /// Module catalog, shared by palette, loader, and the eval engine. `Arc`
    /// so a spawned eval task can hold its own handle to the same catalog.
    pub registry: Arc<Registry>,
    /// The pipe under edit. A fresh session starts on an empty, unsaved pipe.
    pub pipe: Pipe,
    /// Where the pipe was loaded from / will save to; `None` until first save.
    pub path: Option<PathBuf>,
    /// Whether the pipe has unsaved edits.
    pub dirty: bool,
    /// The pane keyboard input is directed at.
    pub focus: Pane,
    /// The node the canvas highlights and the preview will follow.
    pub selected: Option<NodeId>,
    /// Rows of canvas content scrolled off the top.
    pub scroll: u16,
    /// Per-node eval status drawn as the canvas status line.
    pub statuses: BTreeMap<NodeId, NodeReport>,
    /// Whether the help overlay is drawn over the layout.
    pub show_help: bool,
    /// Set once the event loop should tear down and restore the terminal.
    pub should_quit: bool,
    /// Current editor mode — drives status-line hints and key routing.
    pub mode: Mode,
    /// One-line message shown in the status area; replaced by the next action.
    pub status: String,
    /// State for the param-edit overlay (`Some` iff `mode == EditParams`).
    pub edit_state: Option<EditParamsState>,
    /// Live-eval scheduling and per-node loading state (M12).
    pub eval: EvalState,
    /// The preview panel: tab, auto-refresh, scroll, and last snapshot (M13).
    pub preview: PreviewState,
    /// Monotonic tick counter, advanced on every `Msg::Tick`; drives the
    /// canvas spinner animation for loading nodes.
    pub tick_count: u64,
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
        let selected = pipe
            .topo_order()
            .ok()
            .and_then(|order| order.first().copied());
        Self {
            registry: Arc::new(registry),
            pipe,
            path,
            dirty: false,
            focus: Pane::Palette,
            selected,
            scroll: 0,
            statuses: BTreeMap::new(),
            show_help: false,
            should_quit: false,
            mode: Mode::Normal,
            status: String::new(),
            edit_state: None,
            eval: EvalState::default(),
            preview: PreviewState::default(),
            tick_count: 0,
        }
    }

    /// Move the selection one step along the flow, preferring the first
    /// downstream (forward) or upstream (backward) edge neighbor, and falling
    /// back to raw topo order when no edge connects.
    pub fn select_step(&mut self, forward: bool) {
        let Some(sel) = self.selected else {
            self.select_topo_step(forward);
            return;
        };

        if forward {
            // Prefer the first downstream neighbour via an outgoing edge.
            if let Some(next) = self
                .pipe
                .edges
                .iter()
                .find(|e| e.from.node == sel)
                .map(|e| e.to.node)
            {
                self.selected = Some(next);
                return;
            }
        } else {
            // Prefer the first upstream neighbour via an incoming edge.
            if let Some(prev) = self.pipe.edges_into(sel).next().map(|e| e.from.node) {
                self.selected = Some(prev);
                return;
            }
        }

        self.select_topo_step(forward);
    }

    /// Fallback: walk one step in topo order, clamping at the ends.
    fn select_topo_step(&mut self, forward: bool) {
        let Ok(order) = self.pipe.topo_order() else {
            return;
        };
        if order.is_empty() {
            return;
        }
        let next = match self
            .selected
            .and_then(|id| order.iter().position(|&n| n == id))
        {
            Some(i) if forward => order[(i + 1).min(order.len() - 1)],
            Some(i) => order[i.saturating_sub(1)],
            None if forward => order[0],
            None => order[order.len() - 1],
        };
        self.selected = Some(next);
    }

    /// Move the selection left (`right=false`) or right within the same layout
    /// row. No-op if there is no neighbour in that direction.
    pub fn select_lateral(&mut self, right: bool) {
        let Some(sel) = self.selected else { return };
        let layout = Layout::compute(&self.pipe);
        let Some(slot) = layout.slot(sel) else { return };

        let target_col = if right {
            slot.col + 1
        } else {
            slot.col.wrapping_sub(1)
        };
        // wrapping_sub on col==0 gives usize::MAX, which will never match.
        if let Some((id, _)) = layout
            .iter()
            .find(|(_, s)| s.row == slot.row && s.col == target_col)
        {
            self.selected = Some(id);
        }
    }

    /// Jump to the node whose [`NodeId`] badge number equals `n`. No-op if
    /// no such node exists.
    pub fn select_badge(&mut self, n: u64) {
        if self.pipe.nodes.iter().any(|node| node.id.0 == n) {
            self.selected = Some(NodeId(n));
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

    /// Node count, shown in the top bar.
    pub fn node_count(&self) -> usize {
        self.pipe.nodes.len()
    }
}
