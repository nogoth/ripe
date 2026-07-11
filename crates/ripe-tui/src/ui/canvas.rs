//! Center pane: the DAG canvas. Draws the layered auto-layout from
//! [`super::layout`] as boxed nodes joined by box-drawing wires.
//!
//! Everything is painted cell-by-cell into the frame buffer (rather than with
//! nested widgets) so scrolling and clipping are exact and wires can share
//! junctions. The pipeline each frame is: compute the grid layout, auto-scroll
//! to keep the selection in view, paint wires, then paint node boxes on top so
//! a wire can never overwrite a box.
//!
//! Wire routing is deliberately not general Manhattan routing. With layout
//! under our control every edge is a vertical drop down the parent's column
//! plus, when parent and child differ in column, a single horizontal bus in
//! the one-row band directly above the child. Junction glyphs (`├ ┤ ┬ ┴ ┼`
//! and the four corners) fall out of a per-cell direction bitmask, so fan-out
//! and fan-in are the same code.

use std::collections::{HashMap, HashSet};

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use ripe_core::engine::{NodeReport, NodeStatus};
use ripe_core::{NodeId, Params, Pipe};

use crate::app::App;
use crate::ui::layout::{Layout, Slot};
use crate::ui::pane_block;
use crate::ui::theme::Theme;

// --- grid geometry -------------------------------------------------------

/// Box height: top border, two content rows (title, detail+status), bottom
/// border.
const BOX_H: u16 = 4;
/// Rows between vertically stacked boxes — the wire band.
const V_GAP: u16 = 1;
/// Columns between side-by-side boxes.
const H_GAP: u16 = 3;
/// Never shrink a box narrower than this, even in a cramped pane.
const MIN_BOX_W: u16 = 16;

// --- wire direction bits -------------------------------------------------

const UP: u8 = 1;
const DOWN: u8 = 2;
const LEFT: u8 = 4;
const RIGHT: u8 = 8;

pub(crate) fn render(frame: &mut Frame, area: Rect, app: &mut App, focused: bool) {
    let theme = app.theme;
    let block = pane_block("Canvas", focused, &theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    if app.pipe.nodes.is_empty() {
        let hint = Paragraph::new(Line::from(Span::styled(
            "empty pipe — press a to add a node",
            Style::new().fg(theme.text_faint),
        )))
        .alignment(Alignment::Center);
        frame.render_widget(hint, inner);
        return;
    }

    let layout = Layout::compute(&app.pipe);
    let box_w = box_width(inner.width);
    let content_h = content_height(&layout);
    let content_w = content_width(&layout, box_w);

    // Auto-scroll (both axes) so the selected node stays on screen. Rendering
    // owns this (rather than `update`) because it is the only place that
    // knows the pane size; `update` just moves the selection.
    app.scroll = reveal(app, &layout, inner.height as usize, content_h) as u16;
    app.hscroll = reveal_h(app, &layout, box_w, inner.width as usize, content_w) as u16;
    let scroll = app.scroll as usize;
    let hscroll = app.hscroll as usize;

    let mut painter = Painter {
        buf: frame.buffer_mut(),
        inner,
        scroll,
        hscroll,
    };

    draw_wires(&mut painter, &layout, &app.pipe, box_w, &theme);
    let spinner = spinner_frame(app.tick_count);
    let badge_order = layout.badge_order();
    for (id, slot) in layout.iter() {
        let node = app.pipe.node(id).expect("layout ids come from the pipe");
        let badge = badge_order
            .iter()
            .position(|&n| n == id)
            .map_or(0, |i| i + 1);
        draw_box(
            &mut painter,
            badge,
            slot,
            box_w,
            &node.kind,
            &node.params,
            app.statuses.get(&id),
            app.eval.loading.contains(&id),
            spinner,
            app.selected == Some(id),
            &theme,
        );
    }

    let viewport = inner.height as usize;
    if scroll > 0 {
        edge_hint(&mut painter, "↑ more", Edge::Top, &theme);
    }
    if scroll + viewport < content_h {
        edge_hint(&mut painter, "↓ more", Edge::Bottom, &theme);
    }
    if hscroll > 0 {
        edge_hint(&mut painter, "← more", Edge::Left, &theme);
    }
    if hscroll + (inner.width as usize) < content_w {
        edge_hint(&mut painter, "→ more", Edge::Right, &theme);
    }
}

/// The node whose box covers content cell `(cx, cy)`, using the same grid
/// the canvas painted with — this is the mouse hit-test.
pub(crate) fn node_at(pipe: &Pipe, inner_w: u16, cx: usize, cy: usize) -> Option<NodeId> {
    let layout = Layout::compute(pipe);
    let box_w = box_width(inner_w) as usize;
    layout
        .iter()
        .find(|(_, slot)| {
            let x0 = col_x0(slot.col, box_w as u16) as usize;
            let top = box_top(slot.row);
            (x0..x0 + box_w).contains(&cx) && (top..=box_bottom(slot.row)).contains(&cy)
        })
        .map(|(id, _)| id)
}

/// Total height of the laid-out content in cells.
fn content_height(layout: &Layout) -> usize {
    match layout.rows() {
        0 => 0,
        n => n * BOX_H as usize + (n - 1) * V_GAP as usize,
    }
}

/// Total width of the laid-out content in cells (right edge of the rightmost
/// box in any row).
fn content_width(layout: &Layout, box_w: u16) -> usize {
    layout
        .iter()
        .map(|(_, slot)| col_x0(slot.col, box_w) as usize + box_w as usize)
        .max()
        .unwrap_or(0)
}

/// Box width sized so two branches sit side by side in the pane (PLAN.md).
fn box_width(inner_w: u16) -> u16 {
    (inner_w.saturating_sub(H_GAP) / 2)
        .max(MIN_BOX_W)
        .min(inner_w)
}

fn col_x0(col: usize, box_w: u16) -> u16 {
    (col as u16).saturating_mul(box_w + H_GAP)
}

fn center_x(col: usize, box_w: u16) -> u16 {
    col_x0(col, box_w) + box_w / 2
}

fn box_top(row: usize) -> usize {
    row * (BOX_H + V_GAP) as usize
}

fn box_bottom(row: usize) -> usize {
    box_top(row) + BOX_H as usize - 1
}

/// Compute the scroll offset that keeps the selected node visible, clamped so
/// we never scroll past the content. Keeps a one-row margin above and below
/// the selection so the wire arrow and the `more` hints do not cover it.
fn reveal(app: &App, layout: &Layout, viewport: usize, content_h: usize) -> usize {
    if content_h <= viewport {
        return 0;
    }
    let max = content_h - viewport;
    let mut scroll = (app.scroll as usize).min(max);
    if let Some(slot) = app.selected.and_then(|id| layout.slot(id)) {
        let top = box_top(slot.row).saturating_sub(1);
        let bottom = box_bottom(slot.row) + 1;
        if top < scroll {
            scroll = top;
        }
        if bottom >= scroll + viewport {
            scroll = bottom + 1 - viewport;
        }
        scroll = scroll.min(max);
    }
    scroll
}

/// The horizontal counterpart of [`reveal`]: keep the selected node's column
/// in view when a wide pipe (3+ parallel branches) overflows the pane.
fn reveal_h(app: &App, layout: &Layout, box_w: u16, viewport: usize, content_w: usize) -> usize {
    if content_w <= viewport {
        return 0;
    }
    let max = content_w - viewport;
    let mut hscroll = (app.hscroll as usize).min(max);
    if let Some(slot) = app.selected.and_then(|id| layout.slot(id)) {
        let left = col_x0(slot.col, box_w) as usize;
        let right = left + box_w as usize;
        if left < hscroll {
            hscroll = left;
        }
        if right > hscroll + viewport {
            hscroll = right - viewport;
        }
        hscroll = hscroll.min(max);
    }
    hscroll
}

// --- painting ------------------------------------------------------------

/// A clipped, scrolled writer over the frame buffer. All coordinates are in
/// content space (x and y before scrolling); `put` maps them to the screen
/// and drops anything off-pane.
struct Painter<'a> {
    buf: &'a mut Buffer,
    inner: Rect,
    scroll: usize,
    hscroll: usize,
}

impl Painter<'_> {
    fn put(&mut self, cx: u16, cy: usize, ch: char, style: Style) {
        if cy < self.scroll || (cx as usize) < self.hscroll {
            return;
        }
        let dy = cy - self.scroll;
        let dx = cx as usize - self.hscroll;
        if dy >= self.inner.height as usize || dx >= self.inner.width as usize {
            return;
        }
        let x = self.inner.x + dx as u16;
        let y = self.inner.y + dy as u16;
        let mut tmp = [0u8; 4];
        let cell = &mut self.buf[(x, y)];
        cell.set_symbol(ch.encode_utf8(&mut tmp));
        cell.set_style(style);
    }

    fn put_str(&mut self, cx: u16, cy: usize, s: &str, style: Style) {
        let mut x = cx;
        for ch in s.chars() {
            self.put(x, cy, ch, style);
            x += 1;
        }
    }
}

/// Add direction bits to a wire cell.
fn wire(mask: &mut HashMap<(u16, usize), u8>, x: u16, y: usize, bits: u8) {
    *mask.entry((x, y)).or_default() |= bits;
}

fn draw_wires(p: &mut Painter, layout: &Layout, pipe: &Pipe, box_w: u16, theme: &Theme) {
    let mut mask: HashMap<(u16, usize), u8> = HashMap::new();
    let mut arrows: HashSet<(u16, usize)> = HashSet::new();

    for (id, slot) in layout.iter() {
        // Distinct upstream slots feeding this node.
        let mut parents: Vec<Slot> = Vec::new();
        let mut seen = HashSet::new();
        for edge in pipe.edges_into(id) {
            if let Some(ps) = layout.slot(edge.from.node)
                && seen.insert(edge.from.node)
            {
                parents.push(ps);
            }
        }
        if parents.is_empty() {
            continue; // a source: nothing enters it
        }

        let t_cx = center_x(slot.col, box_w);
        let bus_y = box_top(slot.row).saturating_sub(1);

        // A vertical drop down each parent's column to the bus row. The bottom
        // cell stops at the bus (no DOWN) unless it is the child's own column,
        // handled below — so a drop that turns sideways never dangles a stub.
        let mut min_x = t_cx;
        let mut max_x = t_cx;
        for ps in &parents {
            let p_cx = center_x(ps.col, box_w);
            for y in (box_bottom(ps.row) + 1)..=bus_y {
                wire(&mut mask, p_cx, y, UP);
                if y < bus_y {
                    wire(&mut mask, p_cx, y, DOWN);
                }
            }
            min_x = min_x.min(p_cx);
            max_x = max_x.max(p_cx);
        }

        // One horizontal bus in the band above the child, joining every
        // contributing column to the child's column.
        if min_x != max_x {
            for x in min_x..=max_x {
                if x > min_x {
                    wire(&mut mask, x, bus_y, LEFT);
                }
                if x < max_x {
                    wire(&mut mask, x, bus_y, RIGHT);
                }
            }
        }

        // The wire turns down into the child here.
        wire(&mut mask, t_cx, bus_y, DOWN);
        arrows.insert((t_cx, bus_y));
    }

    let line = Style::new().fg(theme.border_dim);
    let head = Style::new().fg(theme.text_dim);
    for (&(x, y), &m) in &mask {
        // An arrowhead only where a wire drops straight in; a junction entry
        // keeps its glyph so the merge stays legible.
        if arrows.contains(&(x, y)) && m & (LEFT | RIGHT) == 0 {
            p.put(x, y, '▼', head);
        } else {
            p.put(x, y, glyph(m), line);
        }
    }
}

/// Box-drawing glyph for a set of direction stubs.
fn glyph(mask: u8) -> char {
    match mask {
        m if m == UP | DOWN => '│',
        m if m == LEFT | RIGHT => '─',
        m if m == DOWN | RIGHT => '┌',
        m if m == DOWN | LEFT => '┐',
        m if m == UP | RIGHT => '└',
        m if m == UP | LEFT => '┘',
        m if m == UP | DOWN | RIGHT => '├',
        m if m == UP | DOWN | LEFT => '┤',
        m if m == DOWN | LEFT | RIGHT => '┬',
        m if m == UP | LEFT | RIGHT => '┴',
        m if m == UP | DOWN | LEFT | RIGHT => '┼',
        m if m == UP || m == DOWN => '│',
        m if m == LEFT || m == RIGHT => '─',
        _ => ' ',
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_box(
    p: &mut Painter,
    badge: usize,
    slot: Slot,
    box_w: u16,
    kind: &str,
    params: &Params,
    report: Option<&NodeReport>,
    loading: bool,
    spinner: char,
    selected: bool,
    theme: &Theme,
) {
    let x0 = col_x0(slot.col, box_w);
    let top = box_top(slot.row);
    let w = box_w;
    if w < 2 {
        return;
    }
    let right = x0 + w - 1;
    let bottom = top + BOX_H as usize - 1;

    // Selected node gets a bright double border; others a rounded border tinted
    // by kind, echoing the mockup's colour-coding.
    let (tl, tr, bl, br, horiz, vert, style) = if selected {
        (
            '╔',
            '╗',
            '╚',
            '╝',
            '═',
            '║',
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
        )
    } else {
        (
            '╭',
            '╮',
            '╰',
            '╯',
            '─',
            '│',
            Style::new().fg(theme.kind_color(kind)),
        )
    };

    p.put(x0, top, tl, style);
    p.put(right, top, tr, style);
    p.put(x0, bottom, bl, style);
    p.put(right, bottom, br, style);
    for x in (x0 + 1)..right {
        p.put(x, top, horiz, style);
        p.put(x, bottom, horiz, style);
    }
    for y in (top + 1)..bottom {
        p.put(x0, y, vert, style);
        p.put(right, y, vert, style);
    }

    let inner_x = x0 + 1;
    let inner_w = (w - 2) as usize;

    // Row 1: numbered badge + kind title.
    let title_style = Style::new()
        .fg(if selected {
            theme.accent
        } else {
            theme.kind_color(kind)
        })
        .add_modifier(Modifier::BOLD);
    let title = truncate(&format!("{badge}  {}", kind_title(kind)), inner_w);
    p.put_str(inner_x, top + 1, &title, title_style);

    // Row 2: param summary on the left, eval status on the right. A node that
    // is currently (re-)evaluating shows a spinner in place of its last status.
    let detail = param_summary(kind, params);
    let status_cell: Option<(String, Style)> = if loading {
        Some((format!("{spinner} …"), Style::new().fg(theme.warn)))
    } else {
        status_summary(report).map(|s| (s, status_style(report, theme)))
    };
    match status_cell {
        Some((status, style)) => {
            let status_w = status.chars().count().min(inner_w);
            let detail_w = inner_w.saturating_sub(status_w + 1);
            p.put_str(
                inner_x,
                top + 2,
                &truncate(&detail, detail_w),
                Style::new().fg(theme.text_dim),
            );
            let sx = inner_x + (inner_w - status_w) as u16;
            p.put_str(sx, top + 2, &status, style);
        }
        None => p.put_str(
            inner_x,
            top + 2,
            &truncate(&detail, inner_w),
            Style::new().fg(theme.text_dim),
        ),
    }
}

/// Braille spinner frames; the app's tick counter selects the phase, so the
/// glyph advances once per event-loop tick while a node is loading. Shared
/// with the preview footer's "evaluating…" indicator.
pub(crate) fn spinner_frame(tick: u64) -> char {
    const FRAMES: [char; 8] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧'];
    FRAMES[(tick as usize) % FRAMES.len()]
}

/// A pane edge a `more` hint can anchor to.
enum Edge {
    Top,
    Bottom,
    Left,
    Right,
}

/// Draw a `↑/↓/←/→ more` hint on a pane edge. Anchoring to a screen edge
/// regardless of scroll means targeting the content cell that currently maps
/// to that edge's first/last visible line or column.
fn edge_hint(p: &mut Painter, text: &str, edge: Edge, theme: &Theme) {
    let w = text.chars().count();
    let (cx, cy) = match edge {
        Edge::Top | Edge::Bottom => {
            let cx = p.hscroll + (p.inner.width as usize).saturating_sub(w) / 2;
            let cy = match edge {
                Edge::Top => p.scroll,
                _ => p.scroll + p.inner.height as usize - 1,
            };
            (cx, cy)
        }
        Edge::Left | Edge::Right => {
            let cx = match edge {
                Edge::Left => p.hscroll,
                _ => p.hscroll + (p.inner.width as usize).saturating_sub(w),
            };
            (cx, p.scroll + p.inner.height as usize / 2)
        }
    };
    p.put_str(cx as u16, cy, text, Style::new().fg(theme.warn));
}

// --- per-node text -------------------------------------------------------

/// A one-line, mockup-flavoured summary of a node's key params. Kept short and
/// truncated by the caller to fit the box.
pub(crate) fn param_summary(kind: &str, params: &Params) -> String {
    match kind {
        "fetch_feed" | "fetch_json" | "fetch_csv" => {
            params.get_str("url").unwrap_or("(no url)").to_string()
        }
        "filter" => params
            .get("rules")
            .and_then(|v| v.as_array())
            .and_then(|rules| rules.first())
            .and_then(|r| r.as_str())
            .unwrap_or("(no rules)")
            .to_string(),
        "sort" => format!(
            "By: {} {}",
            params.get_str("by").unwrap_or("?"),
            params.get_str("order").unwrap_or("asc")
        ),
        "limit" => format!("first {}", num(params)),
        "tail" => format!("last {}", num(params)),
        "regex" => params
            .get_str("pattern")
            .unwrap_or("(no pattern)")
            .to_string(),
        "transform" => {
            let n = params
                .get("ops")
                .and_then(|v| v.as_array())
                .map_or(0, Vec::len);
            format!("{n} op{}", if n == 1 { "" } else { "s" })
        }
        "unique" => format!("by {}", params.get_str("by").unwrap_or("?")),
        "union" => "merge streams".to_string(),
        "reverse" => "reverse order".to_string(),
        "output" => {
            let fmt = params.get_str("format").unwrap_or("rss");
            let dest = params
                .get_str("destination")
                .filter(|d| !d.is_empty())
                .unwrap_or("stdout");
            format!("{fmt} → {dest}")
        }
        _ => String::new(),
    }
}

fn num(params: &Params) -> String {
    params
        .get_usize("n")
        .map_or_else(|| "?".to_string(), |n| n.to_string())
}

/// The status line for a node, or `None` (blank) when there is no report yet.
fn status_summary(report: Option<&NodeReport>) -> Option<String> {
    let report = report?;
    Some(match report.status {
        NodeStatus::Ok | NodeStatus::Cached => match report.item_count {
            Some(n) => format!("✓ {n} items"),
            None => "✓".to_string(),
        },
        NodeStatus::Err => {
            let msg = report.error.as_deref().unwrap_or("error");
            format!("✗ {}", msg.lines().next().unwrap_or("error"))
        }
        NodeStatus::Unready => "· unready".to_string(),
    })
}

fn status_style(report: Option<&NodeReport>, theme: &Theme) -> Style {
    match report.map(|r| &r.status) {
        Some(NodeStatus::Ok | NodeStatus::Cached) => Style::new().fg(theme.ok),
        Some(NodeStatus::Err) => Style::new().fg(theme.err),
        Some(NodeStatus::Unready) => Style::new().fg(theme.text_faint),
        None => Style::new(),
    }
}

fn kind_title(kind: &str) -> &str {
    match kind {
        "fetch_feed" => "Fetch Feed",
        "fetch_json" => "Fetch JSON",
        "fetch_csv" => "Fetch CSV",
        "filter" => "Filter",
        "sort" => "Sort",
        "limit" => "Limit",
        "tail" => "Tail",
        "unique" => "Unique",
        "reverse" => "Reverse",
        "union" => "Union",
        "transform" => "Transform",
        "regex" => "Regex",
        "output" => "Output",
        other => other,
    }
}

/// Clip `s` to `max` display columns, marking a cut with `…`.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut out: String = s.chars().take(max - 1).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn param_summaries_match_their_kinds() {
        let p = Params::new().with("url", "https://x.dev/feed");
        assert_eq!(param_summary("fetch_feed", &p), "https://x.dev/feed");

        let p = Params::new().with("rules", json!(["score > 100", "a == b"]));
        assert_eq!(param_summary("filter", &p), "score > 100");

        let p = Params::new().with("by", "pubDate").with("order", "desc");
        assert_eq!(param_summary("sort", &p), "By: pubDate desc");

        let p = Params::new().with("n", 25);
        assert_eq!(param_summary("limit", &p), "first 25");
        assert_eq!(param_summary("tail", &p), "last 25");

        let p = Params::new().with("pattern", "(?<score>\\d+)");
        assert_eq!(param_summary("regex", &p), "(?<score>\\d+)");

        let p = Params::new().with("ops", json!(["rename a b", "drop c"]));
        assert_eq!(param_summary("transform", &p), "2 ops");

        let p = Params::new().with("format", "rss").with("destination", "");
        assert_eq!(param_summary("output", &p), "rss → stdout");
        let p = Params::new()
            .with("format", "json")
            .with("destination", "out.json");
        assert_eq!(param_summary("output", &p), "json → out.json");
    }

    #[test]
    fn status_lines_cover_every_state() {
        let mk = |status, error, item_count| NodeReport {
            status,
            error,
            duration: std::time::Duration::ZERO,
            item_count,
        };
        assert_eq!(status_summary(None), None);
        assert_eq!(
            status_summary(Some(&mk(NodeStatus::Ok, None, Some(44)))).as_deref(),
            Some("✓ 44 items")
        );
        assert_eq!(
            status_summary(Some(&mk(NodeStatus::Cached, None, Some(3)))).as_deref(),
            Some("✓ 3 items")
        );
        assert_eq!(
            status_summary(Some(&mk(
                NodeStatus::Err,
                Some("boom\nsecond line".to_string()),
                None
            )))
            .as_deref(),
            Some("✗ boom")
        );
        assert_eq!(
            status_summary(Some(&mk(NodeStatus::Unready, None, None))).as_deref(),
            Some("· unready")
        );
    }

    #[test]
    fn spinner_advances_and_wraps_with_the_tick() {
        // Distinct glyphs across a full cycle, wrapping back to the start.
        let a = spinner_frame(0);
        let b = spinner_frame(1);
        assert_ne!(a, b, "consecutive ticks show different frames");
        assert_eq!(spinner_frame(0), spinner_frame(8), "8 frames, then wrap");
    }

    #[test]
    fn node_at_maps_content_cells_to_boxes() {
        // a -> b vertically, in one column.
        let mut pipe = Pipe::new("hit");
        let a = pipe.add_node("fetch_feed", Params::new());
        let b = pipe.add_node("filter", Params::new());
        pipe.connect(a, "out", b, "in");

        let inner_w = 61u16; // box_width = (61 - 3) / 2 = 29
        // Inside a's box (rows 0..=3).
        assert_eq!(node_at(&pipe, inner_w, 0, 0), Some(a));
        assert_eq!(node_at(&pipe, inner_w, 28, 3), Some(a));
        // The wire band row between the boxes hits nothing.
        assert_eq!(node_at(&pipe, inner_w, 5, 4), None);
        // Inside b's box (rows 5..=8).
        assert_eq!(node_at(&pipe, inner_w, 5, 6), Some(b));
        // Right of the single column: nothing.
        assert_eq!(node_at(&pipe, inner_w, 40, 1), None);
    }

    #[test]
    fn truncate_marks_the_cut() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello", 5), "hello");
        assert_eq!(truncate("hello world", 5), "hell…");
        assert_eq!(truncate("hello", 0), "");
    }
}
