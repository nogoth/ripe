//! The view: render the whole editor from an [`App`]. Immediate-mode, so
//! every frame is drawn from scratch off the current model.
//!
//! Layout follows docs/mockup.png: a top bar, a body of three side-by-side
//! panes (palette / canvas / preview, canvas widest), and a status line.
//! Below a usable floor the layout would corrupt, so a size guard swaps in a
//! plain notice instead.

mod canvas;
pub(crate) mod layout;
mod palette;
mod preview;

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};

use crate::app::{App, Pane};

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
    let focus = app.focus;
    palette::render(frame, cols[0], app, focus == Pane::Palette);
    canvas::render(frame, cols[1], app, focus == Pane::Canvas);
    preview::render(frame, cols[2], app, focus == Pane::Preview);

    status_line(frame, rows[2], focus);

    if app.show_help {
        help_overlay(frame, area);
    }
}

/// A bordered pane title block, drawn thick and bright when focused so the
/// active pane reads at a glance.
pub(crate) fn pane_block(title: &str, focused: bool) -> Block<'static> {
    let (border_type, style) = if focused {
        (
            BorderType::Thick,
            Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        )
    } else {
        (BorderType::Plain, Style::new().fg(Color::DarkGray))
    };
    Block::bordered()
        .border_type(border_type)
        .border_style(style)
        .title(title.to_string())
}

fn top_bar(frame: &mut Frame, area: Rect, app: &App) {
    let menu = Line::from(vec![
        Span::styled(
            " PIPES ",
            Style::new()
                .bg(Color::Blue)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  EDIT  RUN  VIEW  HELP"),
    ]);

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

fn status_line(frame: &mut Frame, area: Rect, focus: Pane) {
    let mode = Line::from(vec![
        Span::raw(" Mode: "),
        Span::styled(
            "Normal",
            Style::new().fg(Color::Green).add_modifier(Modifier::BOLD),
        ),
    ]);
    // Contextual keys depend on the focused pane; the canvas advertises its
    // selection motions.
    let keys = match focus {
        Pane::Canvas => "j/k move   tab switch pane   ? help   q quit ",
        _ => "tab switch pane   ? help   q quit ",
    };
    let hints = Line::from(keys).alignment(Alignment::Right);

    let chunks = Layout::horizontal([Constraint::Min(0), Constraint::Length(hints.width() as u16)])
        .split(area);
    frame.render_widget(Paragraph::new(mode), chunks[0]);
    frame.render_widget(Paragraph::new(hints), chunks[1]);
}

fn help_overlay(frame: &mut Frame, area: Rect) {
    let rect = centered_rect(40, 8, area);
    frame.render_widget(Clear, rect);
    let block = Block::bordered()
        .border_type(BorderType::Double)
        .border_style(Style::new().fg(Color::Cyan))
        .title("Help");
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let lines = vec![
        help_line("Tab", "cycle pane focus"),
        help_line("?", "toggle this help"),
        help_line("Esc", "close help"),
        help_line("q / Ctrl-C", "quit"),
    ];
    frame.render_widget(Paragraph::new(lines), inner);
}

fn help_line(key: &str, desc: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!(" {key:<12}"), Style::new().fg(Color::Yellow)),
        Span::raw(desc.to_string()),
    ])
}

fn too_small(frame: &mut Frame, area: Rect) {
    let notice = format!("terminal too small (need {MIN_WIDTH}x{MIN_HEIGHT})");
    let rect = centered_rect(notice.chars().count() as u16, 1, area);
    frame.render_widget(Paragraph::new(notice).alignment(Alignment::Center), rect);
}

/// A `width` x `height` rectangle centered within `area`, clamped to fit.
fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
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
    use ripe_core::{NodeId, Params, Pipe, Registry};
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::time::Duration;

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
}
