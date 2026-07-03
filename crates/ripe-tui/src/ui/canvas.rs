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
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use ripe_core::engine::{NodeReport, NodeStatus};
use ripe_core::{NodeId, Params, Pipe};

use crate::app::App;
use crate::ui::layout::{Layout, Slot};
use crate::ui::pane_block;

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
    let block = pane_block("Canvas", focused);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    if app.pipe.nodes.is_empty() {
        let hint = Paragraph::new(Line::from(Span::styled(
            "empty pipe — press a to add a node",
            Style::new().fg(Color::DarkGray),
        )))
        .alignment(Alignment::Center);
        frame.render_widget(hint, inner);
        return;
    }

    let layout = Layout::compute(&app.pipe);
    let box_w = box_width(inner.width);
    let content_h = content_height(&layout);

    // Auto-scroll so the selected node stays on screen. Rendering owns this
    // (rather than `update`) because it is the only place that knows the pane
    // height; `update` just moves the selection.
    app.scroll = reveal(app, &layout, inner.height as usize, content_h) as u16;
    let scroll = app.scroll as usize;

    let mut painter = Painter {
        buf: frame.buffer_mut(),
        inner,
        scroll,
    };

    draw_wires(&mut painter, &layout, &app.pipe, box_w);
    for (id, slot) in layout.iter() {
        let node = app.pipe.node(id).expect("layout ids come from the pipe");
        draw_box(
            &mut painter,
            id,
            slot,
            box_w,
            &node.kind,
            &node.params,
            app.statuses.get(&id),
            app.selected == Some(id),
        );
    }

    let viewport = inner.height as usize;
    if scroll > 0 {
        edge_hint(&mut painter, "↑ more", true);
    }
    if scroll + viewport < content_h {
        edge_hint(&mut painter, "↓ more", false);
    }
}

/// Total height of the laid-out content in cells.
fn content_height(layout: &Layout) -> usize {
    match layout.rows() {
        0 => 0,
        n => n * BOX_H as usize + (n - 1) * V_GAP as usize,
    }
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

// --- painting ------------------------------------------------------------

/// A clipped, scrolled writer over the frame buffer. All coordinates are in
/// content space (x relative to the pane's left edge, y before scrolling);
/// `put` maps them to the screen and drops anything off-pane.
struct Painter<'a> {
    buf: &'a mut Buffer,
    inner: Rect,
    scroll: usize,
}

impl Painter<'_> {
    fn put(&mut self, cx: u16, cy: usize, ch: char, style: Style) {
        if cy < self.scroll || cx >= self.inner.width {
            return;
        }
        let dy = cy - self.scroll;
        if dy >= self.inner.height as usize {
            return;
        }
        let x = self.inner.x + cx;
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

fn draw_wires(p: &mut Painter, layout: &Layout, pipe: &Pipe, box_w: u16) {
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

    let line = Style::new().fg(Color::DarkGray);
    let head = Style::new().fg(Color::Gray);
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
    id: NodeId,
    slot: Slot,
    box_w: u16,
    kind: &str,
    params: &Params,
    report: Option<&NodeReport>,
    selected: bool,
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
            Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        )
    } else {
        (
            '╭',
            '╮',
            '╰',
            '╯',
            '─',
            '│',
            Style::new().fg(kind_color(kind)),
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
            Color::Cyan
        } else {
            kind_color(kind)
        })
        .add_modifier(Modifier::BOLD);
    let title = truncate(&format!("{}  {}", id.0, kind_title(kind)), inner_w);
    p.put_str(inner_x, top + 1, &title, title_style);

    // Row 2: param summary on the left, eval status on the right.
    let detail = param_summary(kind, params);
    match status_summary(report) {
        Some(status) => {
            let status_w = status.chars().count().min(inner_w);
            let detail_w = inner_w.saturating_sub(status_w + 1);
            p.put_str(
                inner_x,
                top + 2,
                &truncate(&detail, detail_w),
                Style::new().fg(Color::Gray),
            );
            let sx = inner_x + (inner_w - status_w) as u16;
            p.put_str(sx, top + 2, &status, status_style(report));
        }
        None => p.put_str(
            inner_x,
            top + 2,
            &truncate(&detail, inner_w),
            Style::new().fg(Color::Gray),
        ),
    }
}

/// Draw a `↑ more` / `↓ more` hint on the pane's top or bottom edge.
fn edge_hint(p: &mut Painter, text: &str, top: bool) {
    let w = text.chars().count() as u16;
    let cx = (p.inner.width.saturating_sub(w)) / 2;
    // Anchor to a screen edge regardless of scroll by targeting the content
    // row that currently maps to the first/last visible line.
    let cy = if top {
        p.scroll
    } else {
        p.scroll + p.inner.height as usize - 1
    };
    p.put_str(cx, cy, text, Style::new().fg(Color::Yellow));
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

fn status_style(report: Option<&NodeReport>) -> Style {
    match report.map(|r| &r.status) {
        Some(NodeStatus::Ok | NodeStatus::Cached) => Style::new().fg(Color::Green),
        Some(NodeStatus::Err) => Style::new().fg(Color::Red),
        Some(NodeStatus::Unready) => Style::new().fg(Color::DarkGray),
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

fn kind_color(kind: &str) -> Color {
    match kind {
        "fetch_feed" | "fetch_json" | "fetch_csv" | "output" => Color::Blue,
        "regex" | "sort" => Color::Yellow,
        "filter" | "transform" | "union" | "unique" => Color::Magenta,
        "limit" | "tail" | "reverse" => Color::Green,
        _ => Color::Gray,
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
    fn truncate_marks_the_cut() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello", 5), "hello");
        assert_eq!(truncate("hello world", 5), "hell…");
        assert_eq!(truncate("hello", 0), "");
    }
}
