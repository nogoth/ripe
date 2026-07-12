//! The view: render the whole editor from an [`App`]. Immediate-mode, so
//! every frame is drawn from scratch off the current model.
//!
//! Layout follows docs/mockup.png: a top bar, a body of three side-by-side
//! panes (palette / canvas / preview, canvas widest), and a status line.
//! Below a usable floor the layout would corrupt, so a size guard swaps in a
//! plain notice instead.

pub(crate) mod canvas;
pub(crate) mod layout;
pub(crate) mod palette;
mod params;
mod preview;
pub(crate) mod theme;

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};

use crate::actions::{self, Action, Context};
use crate::app::{App, Mode, Pane, PathAction};
use crate::ui::theme::Theme;

/// Smallest terminal we lay the full editor out in. Under this we draw a
/// notice rather than a mangled frame.
pub const MIN_WIDTH: u16 = 80;
pub const MIN_HEIGHT: u16 = 24;

/// Render one frame. Takes `&mut App` because the canvas auto-scrolls to keep
/// the selection visible, which updates `app.scroll` from the pane geometry
/// only the view knows.
pub fn render(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        too_small(frame, area);
        return;
    }

    let rows = Layout::vertical([
        Constraint::Length(1), // top bar
        Constraint::Min(0),    // body
        Constraint::Length(1), // status line
    ])
    .split(area);
    top_bar(frame, rows[0], app);

    // Canvas gets the lion's share; palette is fixed-width, preview takes the
    // rest at half the canvas's weight.
    let cols = Layout::horizontal([
        Constraint::Length(22),
        Constraint::Fill(2),
        Constraint::Fill(1),
    ])
    .split(rows[1]);
    // Record inner (border-excluded) pane rects for mouse hit-testing in
    // `update`; this must track the geometry the pane renderers use.
    app.rects.palette = cols[0].inner(Margin::new(1, 1));
    app.rects.canvas = cols[1].inner(Margin::new(1, 1));
    app.rects.preview = cols[2].inner(Margin::new(1, 1));

    let focus = app.focus;
    palette::render(frame, cols[0], app, focus == Pane::Palette);
    canvas::render(frame, cols[1], app, focus == Pane::Canvas);
    preview::render(frame, cols[2], app, focus == Pane::Preview);

    status_line(frame, rows[2], app);

    if app.show_help {
        help_overlay(frame, area, app);
    }

    // Modal overlays are drawn last so they sit on top of everything.
    if matches!(app.mode, Mode::EditParams(_)) {
        params::render(frame, area, app);
    }
    if let Mode::Command { query, selected } = &app.mode {
        command_overlay(frame, area, app, query, *selected);
    }
}

/// A bordered pane title block, drawn thick and bright when focused so the
/// active pane reads at a glance.
pub(crate) fn pane_block(title: &str, focused: bool, theme: &Theme) -> Block<'static> {
    let (border_type, style) = if focused {
        (
            BorderType::Thick,
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
        )
    } else {
        (BorderType::Plain, Style::new().fg(theme.border_dim))
    };
    Block::bordered()
        .border_type(border_type)
        .border_style(style)
        .title(title.to_string())
}

fn top_bar(frame: &mut Frame, area: Rect, app: &App) {
    let menu = Line::from(Span::styled(
        " PIPES ",
        Style::new()
            .bg(app.theme.menu_bg)
            .fg(app.theme.menu_fg)
            .add_modifier(Modifier::BOLD),
    ));

    let mut name = app.file_label();
    if app.dirty {
        name.push('*');
    }
    let filename = Line::from(Span::styled(
        name,
        Style::new().add_modifier(Modifier::BOLD),
    ))
    .alignment(Alignment::Center);

    let nodes =
        Line::from(format!("\u{25cf} {} nodes ", app.node_count())).alignment(Alignment::Right);

    let chunks = Layout::horizontal([
        Constraint::Length(menu.width() as u16),
        Constraint::Min(0),
        Constraint::Length(nodes.width() as u16),
    ])
    .split(area);
    frame.render_widget(Paragraph::new(menu), chunks[0]);
    frame.render_widget(Paragraph::new(filename), chunks[1]);
    frame.render_widget(Paragraph::new(nodes), chunks[2]);
}

fn status_line(frame: &mut Frame, area: Rect, app: &App) {
    let theme = app.theme;
    // Path prompt: the whole status line becomes a single-line text input.
    if let Mode::PromptPath { action, buf } = &app.mode {
        let label = match action {
            PathAction::Save => "Save to: ",
            PathAction::Open => "Open: ",
        };
        let text = format!(" {label}{buf}█");
        frame.render_widget(
            Paragraph::new(text).style(Style::new().fg(theme.text)),
            area,
        );
        return;
    }

    // Normal / other modes: left side shows the mode tag and status message,
    // right side shows contextual key hints.
    let (mode_label, mode_color) = match &app.mode {
        Mode::Normal => ("Normal", theme.ok),
        Mode::InsertPending => ("Insert", theme.warn),
        Mode::Connecting { .. } => ("Connect", theme.accent),
        Mode::QuitGuard => ("Quit?", theme.err),
        Mode::EditParams(_) => ("Params", theme.modal),
        Mode::Command { .. } => ("Command", theme.modal),
        Mode::PromptPath { .. } => unreachable!(),
    };

    let left = Line::from(vec![
        Span::raw(" Mode: "),
        Span::styled(
            mode_label,
            Style::new().fg(mode_color).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::raw(app.status.clone()),
    ]);

    let keys = hint_line(app);
    let hints = Line::from(keys).alignment(Alignment::Right);

    let chunks = Layout::horizontal([Constraint::Min(0), Constraint::Length(hints.width() as u16)])
        .split(area);
    frame.render_widget(Paragraph::new(left), chunks[0]);
    frame.render_widget(Paragraph::new(hints), chunks[1]);
}

/// Contextual key hints, generated from the live keymap so a remapped key
/// never leaves the status line lying about it.
fn hint_line(app: &App) -> String {
    let k = |a: Action| app.keymap.label(a);
    if matches!(app.mode, Mode::EditParams(_)) {
        // Enter applies from single-line fields; in the rule list it's a
        // newline, so don't advertise it as apply there.
        let multiline = matches!(
            app.edit_state
                .as_ref()
                .and_then(|s| s.editors.get(s.focused)),
            Some(crate::app::FieldEditor::RuleList(_))
        );
        return if multiline {
            "Ctrl-s apply   Enter new line   Esc cancel   ↑/↓ field ".to_string()
        } else {
            "Enter/Ctrl-s apply   Esc cancel   ↑/↓ field ".to_string()
        };
    }
    if matches!(app.mode, Mode::Command { .. }) {
        return "type to filter   ↑/↓ select   Enter run   Esc close ".to_string();
    }
    match app.focus {
        Pane::Canvas => format!(
            "{} insert   {} del   {} connect   {} run   {}/{} move   tab pane   ? help   {} quit ",
            k(Action::Insert),
            k(Action::DeleteNode),
            k(Action::Connect),
            k(Action::RunAll),
            k(Action::StepNext),
            k(Action::StepPrev),
            k(Action::Quit),
        ),
        Pane::Preview => format!(
            "{} {} tab   {} auto-refresh   {}/{} scroll   {} run   tab pane   ? help ",
            k(Action::PrevTab),
            k(Action::NextTab),
            k(Action::ToggleAutoRefresh),
            k(Action::ScrollDown),
            k(Action::ScrollUp),
            k(Action::RunAll),
        ),
        Pane::Palette => format!(
            "{} insert   {} palette   tab switch pane   ? help   {} quit ",
            k(Action::Insert),
            k(Action::CommandPalette),
            k(Action::Quit),
        ),
    }
}

/// The keymap reference, generated from the action catalog: GLOBAL bindings
/// in the left column, CANVAS and PREVIEW in the right, plus the modal keys
/// that live outside the catalog. Labels come from the live keymap, so
/// config remaps show up here automatically.
fn help_overlay(frame: &mut Frame, area: Rect, app: &App) {
    let theme = app.theme;
    let catalog_rows = |ctx: Context| -> Vec<Line<'static>> {
        let mut lines = vec![Line::from(Span::styled(
            format!(" {}", ctx.heading()),
            Style::new().fg(theme.text_dim).add_modifier(Modifier::BOLD),
        ))];
        for info in actions::CATALOG.iter().filter(|i| i.context == ctx) {
            lines.push(help_line(&app.keymap.label(info.action), info.name, &theme));
        }
        lines
    };

    let mut left = catalog_rows(Context::Global);
    left.push(Line::raw(""));
    left.push(Line::from(Span::styled(
        " MODES",
        Style::new().fg(theme.text_dim).add_modifier(Modifier::BOLD),
    )));
    left.push(help_line("Esc", "dismiss / cancel", &theme));
    left.push(help_line("Ctrl-c", "quit immediately", &theme));

    let mut right = catalog_rows(Context::Canvas);
    right.push(help_line("1-9", "jump to node badge", &theme));
    right.push(help_line("a <letter>", "insert palette kind", &theme));
    right.push(Line::raw(""));
    right.extend(catalog_rows(Context::Preview));

    let rows = left.len().max(right.len()) as u16;
    let rect = centered_rect(76.min(area.width.saturating_sub(2)), rows + 2, area);
    frame.render_widget(Clear, rect);
    let block = Block::bordered()
        .border_type(BorderType::Double)
        .border_style(Style::new().fg(theme.accent))
        .title("Help — keymap");
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let cols =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).split(inner);
    frame.render_widget(Paragraph::new(left), cols[0]);
    frame.render_widget(Paragraph::new(right), cols[1]);
}

fn help_line(key: &str, desc: &str, theme: &Theme) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!(" {key:<12}"), Style::new().fg(theme.warn)),
        Span::raw(desc.to_string()),
    ])
}

/// The command palette: a centered overlay with a query line and the
/// fuzzy-filtered action list, key labels right-aligned.
fn command_overlay(frame: &mut Frame, area: Rect, app: &App, query: &str, selected: usize) {
    let theme = app.theme;
    let filtered = actions::filtered_actions(query);
    let list_h = (filtered.len() as u16).clamp(1, 12);
    let width = 48.min(area.width.saturating_sub(2));
    let rect = centered_rect(width, list_h + 3, area);
    frame.render_widget(Clear, rect);
    let block = Block::bordered()
        .border_type(BorderType::Double)
        .border_style(Style::new().fg(theme.accent))
        .title("Command");
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let mut lines = vec![Line::from(vec![
        Span::styled(" > ", Style::new().fg(theme.accent)),
        Span::styled(format!("{query}█"), Style::new().fg(theme.text)),
    ])];
    if filtered.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no matching command",
            Style::new().fg(theme.text_faint),
        )));
    }
    let inner_w = inner.width as usize;
    // Keep the selection visible when the list is longer than the box.
    let visible = list_h as usize;
    let offset = selected.saturating_sub(visible - 1);
    for (i, info) in filtered.iter().enumerate().skip(offset).take(visible) {
        let label = app.keymap.label(info.action);
        let name = format!(" {} {}", if i == selected { "▶" } else { " " }, info.name);
        let pad = inner_w.saturating_sub(name.chars().count() + label.chars().count() + 1);
        let style = if i == selected {
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(theme.text_dim)
        };
        lines.push(Line::from(vec![
            Span::styled(name, style),
            Span::raw(" ".repeat(pad)),
            Span::styled(label, Style::new().fg(theme.warn)),
        ]));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

fn too_small(frame: &mut Frame, area: Rect) {
    let notice = format!("terminal too small (need {MIN_WIDTH}x{MIN_HEIGHT})");
    let rect = centered_rect(notice.chars().count() as u16, 1, area);
    frame.render_widget(Paragraph::new(notice).alignment(Alignment::Center), rect);
}

/// A `width` x `height` rectangle centered within `area`, clamped to fit.
pub(crate) fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ripe_core::engine::{NodeReport, NodeStatus};
    use ripe_core::{Item, NodeId, Params, Pipe, Registry};
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::time::Duration;

    use crate::app::{EditParamsState, FieldEditor, Mode, PreviewTab};
    use ripe_core::preview::Preview;

    /// A small but branch-free pipe: fetch_feed -> filter -> output.
    fn sample_pipe() -> Pipe {
        let mut pipe = Pipe::new("news_pipeline");
        let src = pipe.add_node(
            "fetch_feed",
            Params::new().with("url", "https://example.com/rss"),
        );
        let filter = pipe.add_node("filter", Params::new());
        let output = pipe.add_node("output", Params::new().with("format", "rss"));
        pipe.connect(src, "out", filter, "in");
        pipe.connect(filter, "out", output, "in");
        pipe
    }

    /// The mockup's graph (docs/mockup.png): a long branch
    /// fetch -> filter -> regex -> filter, a second fetch, both merging at a
    /// union, then sort -> output.
    fn mockup_pipe() -> Pipe {
        let mut pipe = Pipe::new("news_pipeline");
        let fetch1 = pipe.add_node(
            "fetch_feed",
            Params::new().with("url", "https://hnrss.org/frontpage"),
        );
        let filter1 = pipe.add_node(
            "filter",
            Params::new().with("rules", serde_json::json!(["item.pubDate > now - 2d"])),
        );
        let regex = pipe.add_node(
            "regex",
            Params::new()
                .with("pattern", "^\\[(?<score>\\d+)\\]\\s(?<title>.*)")
                .with("mode", "extract"),
        );
        let filter2 = pipe.add_node(
            "filter",
            Params::new().with("rules", serde_json::json!(["score > 100"])),
        );
        let fetch2 = pipe.add_node(
            "fetch_feed",
            Params::new().with("url", "https://www.reddit.com/r/rust/.rss"),
        );
        let union = pipe.add_node("union", Params::new());
        let sort = pipe.add_node(
            "sort",
            Params::new().with("by", "pubDate").with("order", "desc"),
        );
        let output = pipe.add_node(
            "output",
            Params::new()
                .with("format", "rss")
                .with("destination", "temp://output.xml"),
        );
        pipe.connect(fetch1, "out", filter1, "in");
        pipe.connect(filter1, "out", regex, "in");
        pipe.connect(regex, "out", filter2, "in");
        pipe.connect(filter2, "out", union, "in");
        pipe.connect(fetch2, "out", union, "in");
        pipe.connect(union, "out", sort, "in");
        pipe.connect(sort, "out", output, "in");
        pipe
    }

    /// Fabricate an all-`Ok` report with the given item count.
    fn ok(count: usize) -> NodeReport {
        NodeReport {
            status: NodeStatus::Ok,
            error: None,
            duration: Duration::ZERO,
            item_count: Some(count),
        }
    }

    /// Draw `app` into an off-screen backend and flatten the buffer into rows
    /// of text (trailing blanks trimmed) — a readable grid, not a cell dump.
    fn render_to_string(app: &mut App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| render(frame, app)).unwrap();
        buffer_to_string(terminal.backend().buffer())
    }

    fn buffer_to_string(buffer: &Buffer) -> String {
        let area = buffer.area;
        let mut out = String::new();
        for y in 0..area.height {
            let mut line = String::new();
            for x in 0..area.width {
                line.push_str(buffer[(x, y)].symbol());
            }
            out.push_str(line.trim_end());
            out.push('\n');
        }
        out
    }

    #[test]
    fn full_layout_with_a_loaded_pipe() {
        let mut app = App::with_pipe(
            Registry::with_builtins(),
            sample_pipe(),
            PathBuf::from("news_pipeline.pipe"),
        );
        insta::assert_snapshot!(render_to_string(&mut app, 120, 40));
    }

    #[test]
    fn too_small_notice() {
        let mut app = App::new(Registry::with_builtins());
        insta::assert_snapshot!(render_to_string(&mut app, 60, 15));
    }

    #[test]
    fn help_overlay_open() {
        let mut app = App::with_pipe(
            Registry::with_builtins(),
            sample_pipe(),
            PathBuf::from("news_pipeline.pipe"),
        );
        app.show_help = true;
        insta::assert_snapshot!(render_to_string(&mut app, 120, 40));
    }

    /// The acceptance bar: the mockup-shaped pipe with fabricated item counts
    /// so status lines show. Read this snapshot for aligned boxes, clean wires,
    /// and a visible branch/merge.
    #[test]
    fn mockup_pipe_canvas() {
        let pipe = mockup_pipe();
        let ids: Vec<NodeId> = pipe.nodes.iter().map(|n| n.id).collect();
        let counts = [44, 32, 32, 12, 44, 44, 44, 44];
        let statuses: BTreeMap<NodeId, NodeReport> =
            ids.iter().zip(counts).map(|(&id, c)| (id, ok(c))).collect();

        let mut app = App::with_pipe(
            Registry::with_builtins(),
            pipe,
            PathBuf::from("news_pipeline.pipe"),
        );
        app.focus = Pane::Canvas;
        app.selected = Some(ids[0]); // highlight the head of the main branch
        app.statuses = statuses;
        insta::assert_snapshot!(render_to_string(&mut app, 120, 40));
    }

    /// A run in flight: the two source nodes are mid-fetch and show a spinner
    /// in place of a status; the rest carry their last item counts.
    #[test]
    fn canvas_shows_spinner_on_loading_nodes() {
        let pipe = mockup_pipe();
        let ids: Vec<NodeId> = pipe.nodes.iter().map(|n| n.id).collect();
        let counts = [44, 32, 32, 12, 44, 44, 44, 44];
        let statuses: BTreeMap<NodeId, NodeReport> =
            ids.iter().zip(counts).map(|(&id, c)| (id, ok(c))).collect();

        let mut app = App::with_pipe(
            Registry::with_builtins(),
            pipe,
            PathBuf::from("news_pipeline.pipe"),
        );
        app.focus = Pane::Canvas;
        app.selected = Some(ids[0]);
        app.statuses = statuses;
        // The two Fetch Feed sources (indices 0 and 4) are still fetching.
        app.eval.loading = [ids[0], ids[4]].into_iter().collect();
        app.tick_count = 0; // pin the spinner to its first frame
        insta::assert_snapshot!(render_to_string(&mut app, 120, 40));
    }

    /// A tall linear pipe with the selection at the bottom: exercises vertical
    /// scroll and the `↑ more` clip indicator.
    #[test]
    fn tall_pipe_scrolled_to_bottom() {
        let mut pipe = Pipe::new("tall");
        let mut prev = pipe.add_node(
            "fetch_feed",
            Params::new().with("url", "https://x.dev/feed"),
        );
        for _ in 0..9 {
            let next = pipe.add_node("filter", Params::new());
            pipe.connect(prev, "out", next, "in");
            prev = next;
        }
        let output = pipe.add_node("output", Params::new().with("format", "rss"));
        pipe.connect(prev, "out", output, "in");

        let mut app = App::with_pipe(Registry::with_builtins(), pipe, PathBuf::from("tall.pipe"));
        app.focus = Pane::Canvas;
        app.selected = Some(output);
        insta::assert_snapshot!(render_to_string(&mut app, 120, 40));
    }

    // --- M11 snapshot test -----------------------------------------------

    /// The param-edit overlay open on a filter node with one valid rule and
    /// one invalid rule. The invalid rule error must be visible in the snapshot.
    ///
    /// State is constructed directly (not via key events) so the snapshot is
    /// deterministic regardless of tui-textarea cursor-blink state.
    #[test]
    fn params_overlay_open_with_inline_error() {
        let mut pipe = Pipe::new("snap");
        let filter_id = pipe.add_node(
            "filter",
            Params::new().with("rules", serde_json::json!(["score > 100"])),
        );
        let mut app = App::with_pipe(Registry::with_builtins(), pipe, PathBuf::from("snap.pipe"));
        app.focus = Pane::Canvas;
        app.selected = Some(filter_id);

        // Build the overlay state manually so the textarea content is stable.
        let registry = Registry::with_builtins();
        let mut state =
            EditParamsState::open(filter_id, &registry, &app.pipe).expect("state must be created");

        // Replace the rules RuleList textarea with two lines: valid + invalid.
        if let Some(FieldEditor::RuleList(ta)) = state.editors.first_mut() {
            *ta = tui_textarea::TextArea::new(vec![
                "score > 100".to_string(),
                "score >> 1".to_string(),
            ]);
        }
        // Inject the inline error that a failed apply would have produced.
        state.field_errors[0] = Some("line 2: unknown operator `>>`".to_string());

        app.edit_state = Some(state);
        app.mode = Mode::EditParams(filter_id);

        insta::assert_snapshot!(render_to_string(&mut app, 120, 40));
    }

    // --- M13 preview snapshot tests --------------------------------------

    /// A mockup-flavoured preview snapshot built off a pinned clock so ages
    /// (and the footer time) are deterministic.
    fn mockup_preview() -> Preview {
        let now = ripe_core::expr::parse_date("2026-07-05T12:00:00+00:00").unwrap();
        let mk = |title: &str, link: &str, desc: &str, pubdate: &str| {
            Item(
                serde_json::json!({
                    "title": title,
                    "link": link,
                    "description": desc,
                    "pubDate": pubdate,
                })
                .as_object()
                .unwrap()
                .clone(),
            )
        };
        let items = vec![
            mk(
                "Introducing Rust 1.78",
                "https://blog.rust-lang.org/2026/rust-1-78.html",
                "Rust 1.78.0 is now available! This release includes performance improvements across the board.",
                "2026-07-05T10:00:00+00:00",
            ),
            mk(
                "Ask HN: What's a technology that didn't live up to the hype?",
                "https://news.ycombinator.com/item?id=42",
                "I'm looking for examples of technologies that had a lot of promise but never really delivered.",
                "2026-07-05T09:00:00+00:00",
            ),
            mk(
                "The problem with async in Rust",
                "https://smallcultfollowing.com/babysteps/async",
                "Async Rust is powerful but it comes with complexity that isn't always worth the trouble.",
                "2026-07-05T07:00:00+00:00",
            ),
        ];
        Preview::build(&items, ripe_core::Format::Rss, "Temp RSS", now)
    }

    fn preview_app(tab: PreviewTab) -> App {
        let mut app = App::with_pipe(
            Registry::with_builtins(),
            mockup_pipe(),
            PathBuf::from("news_pipeline.pipe"),
        );
        app.focus = Pane::Preview;
        app.preview.tab = tab;
        app.preview.snapshot = Some(mockup_preview());
        app
    }

    #[test]
    fn preview_items_tab() {
        let mut app = preview_app(PreviewTab::Items);
        insta::assert_snapshot!(render_to_string(&mut app, 160, 40));
    }

    #[test]
    fn preview_feed_tab() {
        let mut app = preview_app(PreviewTab::Feed);
        insta::assert_snapshot!(render_to_string(&mut app, 160, 40));
    }

    #[test]
    fn preview_raw_tab() {
        let mut app = preview_app(PreviewTab::Raw);
        insta::assert_snapshot!(render_to_string(&mut app, 160, 40));
    }

    /// Auto-refresh off + a fetch failure: the panel names the failing node
    /// instead of showing stale output, and the footer reads "off".
    #[test]
    fn preview_error_state() {
        let mut app = preview_app(PreviewTab::Items);
        app.preview.snapshot = None;
        app.preview.error =
            Some("#1 fetch_feed: 404 Not Found (https://hnrss.org/frontpage)".to_string());
        app.preview.auto_refresh = false;
        insta::assert_snapshot!(render_to_string(&mut app, 160, 40));
    }

    /// A run in flight with no prior snapshot: the footer shows the spinner.
    #[test]
    fn preview_running_shows_spinner_footer() {
        let mut app = preview_app(PreviewTab::Items);
        app.preview.snapshot = None;
        app.eval.running = true;
        app.tick_count = 1; // pin the spinner frame
        insta::assert_snapshot!(render_to_string(&mut app, 160, 40));
    }

    // --- M14: horizontal overflow + command palette -----------------------

    /// Five parallel sources feeding one union: wide enough to overflow the
    /// canvas pane at 120 columns.
    fn wide_pipe() -> Pipe {
        let mut pipe = Pipe::new("wide");
        let union = {
            let sources: Vec<NodeId> = (0..5)
                .map(|i| {
                    pipe.add_node(
                        "fetch_feed",
                        Params::new().with("url", format!("https://s{i}.dev/rss")),
                    )
                })
                .collect();
            let union = pipe.add_node("union", Params::new());
            for src in sources {
                pipe.connect(src, "out", union, "in");
            }
            union
        };
        let output = pipe.add_node("output", Params::new());
        pipe.connect(union, "out", output, "in");
        pipe
    }

    /// Selection on the leftmost source: the canvas shows a `→ more` hint and
    /// clips the overflowing right columns.
    #[test]
    fn wide_pipe_overflows_right_with_a_more_hint() {
        let mut app = App::with_pipe(
            Registry::with_builtins(),
            wide_pipe(),
            PathBuf::from("wide.pipe"),
        );
        app.focus = Pane::Canvas;
        let rendered = render_to_string(&mut app, 120, 40);
        assert!(rendered.contains("→ more"), "right overflow hint missing");
        assert!(!rendered.contains("← more"), "nothing clipped on the left");
        insta::assert_snapshot!(rendered);
    }

    /// Selecting the rightmost source auto-scrolls horizontally: the left
    /// hint appears, and the selected box is on screen (double border).
    #[test]
    fn wide_pipe_hscrolls_to_reveal_the_selection() {
        let mut app = App::with_pipe(
            Registry::with_builtins(),
            wide_pipe(),
            PathBuf::from("wide.pipe"),
        );
        app.focus = Pane::Canvas;
        // The rightmost source has the highest column; ids 1..=5 are the
        // sources in insertion order, and columns follow NodeId tie-break.
        app.selected = Some(NodeId(5));
        let rendered = render_to_string(&mut app, 120, 40);
        assert!(rendered.contains("← more"), "left overflow hint missing");
        assert!(app.hscroll > 0, "render must advance hscroll");
        assert!(
            rendered.contains('╔'),
            "selected box must be visible after hscroll"
        );
    }

    #[test]
    fn command_palette_overlay_lists_filtered_actions() {
        let mut app = App::with_pipe(
            Registry::with_builtins(),
            sample_pipe(),
            PathBuf::from("news_pipeline.pipe"),
        );
        app.mode = Mode::Command {
            query: "run".to_string(),
            selected: 0,
        };
        insta::assert_snapshot!(render_to_string(&mut app, 120, 40));
    }
}
