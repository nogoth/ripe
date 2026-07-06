//! Right pane: the output preview. Three tabs over the Output node's stream —
//! FEED (channel metadata), ITEMS (a card per item), RAW (the serialized
//! feed) — with an auto-refresh toggle and a "Rendered N items" footer.
//!
//! The panel is a pure function of [`App::preview`]: the snapshot is built off
//! the render thread on each eval (see [`ripe_core::preview`]), so drawing
//! never serializes a feed or reads a clock. Scroll is the one piece of state
//! the view owns, clamped here against the body height.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Tabs, Wrap};

use ripe_core::preview::Preview;

use crate::app::{App, PreviewTab};
use crate::ui::canvas::spinner_frame;
use crate::ui::pane_block;
use crate::ui::theme::Theme;

pub(crate) fn render(frame: &mut Frame, area: Rect, app: &mut App, focused: bool) {
    // The block title echoes the mockup's "PREVIEW: <feed>" once a run has
    // produced a stream.
    let title = match &app.preview.snapshot {
        Some(snap) => format!("Preview: {}", snap.feed.title),
        None => "Preview".to_string(),
    };
    let theme = app.theme;
    let block = pane_block(&title, focused, &theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let rows = Layout::vertical([
        Constraint::Length(1), // tab bar
        Constraint::Min(0),    // body
        Constraint::Length(1), // footer
    ])
    .split(inner);

    render_tabs(frame, rows[0], app);
    render_body(frame, rows[1], app);
    render_footer(frame, rows[2], app);
}

fn render_tabs(frame: &mut Frame, area: Rect, app: &App) {
    let count = app.preview.snapshot.as_ref().map_or(0, |s| s.count);
    let titles = vec![
        "FEED".to_string(),
        format!("ITEMS ({count})"),
        "RAW".to_string(),
    ];
    let theme = app.theme;
    let tabs = Tabs::new(titles)
        .select(app.preview.tab.index())
        .highlight_style(Style::new().fg(theme.accent).add_modifier(Modifier::BOLD))
        .style(Style::new().fg(theme.text_faint))
        .divider("  ");
    frame.render_widget(tabs, area);
}

fn render_body(frame: &mut Frame, area: Rect, app: &mut App) {
    // An error (or empty-state note) replaces the body for every tab: never
    // show stale output next to a failure.
    let theme = app.theme;
    if let Some(err) = &app.preview.error {
        notice(frame, area, err, theme.err);
        return;
    }
    let height = area.height as usize;
    let width = area.width as usize;
    let lines: Vec<Line<'static>> = match &app.preview.snapshot {
        None => {
            notice(
                frame,
                area,
                "No preview yet — press r to run the pipe.",
                theme.text_faint,
            );
            return;
        }
        Some(snap) => match app.preview.tab {
            PreviewTab::Feed => feed_lines(snap, &theme),
            PreviewTab::Items => item_lines(snap, width, &theme),
            PreviewTab::Raw => raw_lines(snap, &theme),
        },
    };

    // Clamp the offset to the last page and write it back, so `G` lands
    // exactly at the bottom and edits that shrink the body cannot strand the
    // viewport past the end.
    let max_scroll = lines.len().saturating_sub(height) as u16;
    if app.preview.scroll > max_scroll {
        app.preview.scroll = max_scroll;
    }
    let scroll = app.preview.scroll;
    frame.render_widget(Paragraph::new(lines).scroll((scroll, 0)), area);
}

fn render_footer(frame: &mut Frame, area: Rect, app: &App) {
    let theme = app.theme;
    let dim = Style::new().fg(theme.text_faint);
    let width = area.width as usize;

    // Right: the auto-refresh toggle, always shown in full and right-aligned.
    let right = format!(
        "Auto refresh: {} ",
        if app.preview.auto_refresh {
            "on"
        } else {
            "off"
        }
    );
    let right_w = right.chars().count();

    // Left: a run indicator, "Rendered N items · <time>", or an error note.
    // Coloured, and truncated to whatever the right side leaves free so the
    // two halves never collide in a narrow pane.
    let (left, left_style) = if app.eval.running {
        // A run is in flight — a source fetch may be mid-flight. Spin.
        (
            format!(" {} evaluating…", spinner_frame(app.tick_count)),
            Style::new().fg(theme.warn),
        )
    } else if let Some(snap) = &app.preview.snapshot {
        (
            format!(
                " Rendered {} item{} · {}",
                snap.count,
                if snap.count == 1 { "" } else { "s" },
                snap.updated_label()
            ),
            dim,
        )
    } else if app.preview.error.is_some() {
        (" eval error".to_string(), Style::new().fg(theme.err))
    } else {
        (" Rendered 0 items".to_string(), dim)
    };

    let avail = width.saturating_sub(right_w);
    let left = truncate(&left, avail);
    let pad = avail.saturating_sub(left.chars().count());
    let spans = vec![
        Span::styled(left, left_style),
        Span::raw(" ".repeat(pad)),
        Span::styled(right, dim),
    ];
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

// --- tab bodies ----------------------------------------------------------

/// FEED: channel-level metadata as a key/value list.
fn feed_lines(snap: &Preview, theme: &Theme) -> Vec<Line<'static>> {
    let meta = &snap.feed;
    vec![
        Line::raw(""),
        kv("Title", &meta.title, theme),
        kv("Link", meta.link.as_deref().unwrap_or("—"), theme),
        kv("Description", &meta.description, theme),
        kv("Format", meta.format.name(), theme),
        kv("Items", &snap.count.to_string(), theme),
        Line::raw(""),
        Line::from(Span::styled(
            format!(" Last eval {}", snap.updated_label()),
            Style::new().fg(theme.text_faint),
        )),
    ]
}

fn kv(label: &str, value: &str, theme: &Theme) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!(" {label:<12}"), Style::new().fg(theme.text_faint)),
        Span::raw(value.to_string()),
    ])
}

/// ITEMS: a card per item — bullet + bold title with a right-aligned age,
/// then the domain, a blank line, and up to two wrapped snippet lines.
fn item_lines(snap: &Preview, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    if snap.cards.is_empty() {
        return vec![
            Line::raw(""),
            Line::from(Span::styled(
                " (no items)",
                Style::new().fg(theme.text_faint),
            )),
        ];
    }
    let mut lines = Vec::new();
    for (i, card) in snap.cards.iter().enumerate() {
        if i > 0 {
            // A faint divider between cards, echoing the mockup.
            lines.push(Line::from(Span::styled(
                "─".repeat(width),
                Style::new().fg(theme.divider),
            )));
        }
        lines.push(title_line(&card.title, &card.age, width, theme));
        if !card.domain.is_empty() {
            lines.push(Line::from(Span::styled(
                format!("  {}", card.domain),
                Style::new().fg(theme.text_faint),
            )));
        }
        if !card.snippet.is_empty() {
            lines.push(Line::raw(""));
            for wrapped in wrap(&card.snippet, width.saturating_sub(2), 2) {
                lines.push(Line::from(Span::styled(
                    format!("  {wrapped}"),
                    Style::new().fg(theme.text_dim),
                )));
            }
        }
    }
    lines
}

/// The title line: ` ● <title>` on the left, the age right-aligned. The title
/// is truncated so the age always fits.
fn title_line(title: &str, age: &str, width: usize, theme: &Theme) -> Line<'static> {
    const LEAD: usize = 3; // " ● "
    const TRAIL: usize = 1; // a right margin
    let age_w = age.chars().count();
    let gap = if age_w > 0 { 1 } else { 0 };
    let title_max = width.saturating_sub(LEAD + TRAIL + age_w + gap);
    let title = truncate(title, title_max);
    let title_w = title.chars().count();

    let mut spans = vec![
        Span::raw(" "),
        Span::styled("●", Style::new().fg(theme.accent)),
        Span::raw(" "),
        Span::styled(title, Style::new().add_modifier(Modifier::BOLD)),
    ];
    if age_w > 0 {
        let pad = width.saturating_sub(LEAD + title_w + TRAIL + age_w).max(1);
        spans.push(Span::raw(" ".repeat(pad)));
        spans.push(Span::styled(
            age.to_string(),
            Style::new().fg(theme.text_faint),
        ));
    }
    Line::from(spans)
}

/// RAW: the serialized feed, one screen line per source line.
fn raw_lines(snap: &Preview, theme: &Theme) -> Vec<Line<'static>> {
    snap.raw
        .lines()
        .map(|l| Line::from(Span::styled(l.to_string(), Style::new().fg(theme.text_dim))))
        .collect()
}

// --- helpers -------------------------------------------------------------

/// A centered, wrapped notice filling the body (error / empty states).
fn notice(frame: &mut Frame, area: Rect, message: &str, color: Color) {
    let para = Paragraph::new(message.to_string())
        .style(Style::new().fg(color))
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true });
    // Nudge down a row so the text is not jammed against the tab bar.
    let body = Rect {
        y: area.y + area.height.min(1),
        height: area.height.saturating_sub(1),
        ..area
    };
    frame.render_widget(para, body);
}

/// Greedy word-wrap to `width`, capped at `max_lines`; a truncated tail is
/// marked with an ellipsis. Over-long single words are hard-cut.
fn wrap(text: &str, width: usize, max_lines: usize) -> Vec<String> {
    if width == 0 || max_lines == 0 {
        return Vec::new();
    }
    let words: Vec<&str> = text.split_whitespace().collect();
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut i = 0;
    while i < words.len() {
        let word = words[i];
        if cur.is_empty() {
            cur = word.chars().take(width).collect();
            i += 1;
        } else if cur.chars().count() + 1 + word.chars().count() <= width {
            cur.push(' ');
            cur.push_str(word);
            i += 1;
        } else {
            lines.push(std::mem::take(&mut cur));
            if lines.len() == max_lines {
                break;
            }
        }
    }
    if lines.len() < max_lines && !cur.is_empty() {
        lines.push(cur);
    }
    // Words left over means we ran out of lines: mark the cut.
    if i < words.len()
        && let Some(last) = lines.last_mut()
    {
        let mut chars: Vec<char> = last.chars().collect();
        if chars.len() >= width {
            chars.truncate(width.saturating_sub(1));
        }
        last.clear();
        last.extend(chars);
        last.push('…');
    }
    lines
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

    #[test]
    fn wrap_caps_lines_and_marks_truncation() {
        let text = "the quick brown fox jumps over the lazy dog again and again";
        let lines = wrap(text, 12, 2);
        assert_eq!(lines.len(), 2);
        assert!(
            lines.last().unwrap().ends_with('…'),
            "truncated tail must end with …: {lines:?}"
        );
        for l in &lines {
            assert!(l.chars().count() <= 12, "line over width: {l:?}");
        }
    }

    #[test]
    fn wrap_without_truncation_has_no_ellipsis() {
        let lines = wrap("a short line", 40, 3);
        assert_eq!(lines, vec!["a short line".to_string()]);
    }

    #[test]
    fn truncate_marks_the_cut() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 5), "hell…");
        assert_eq!(truncate("x", 0), "");
    }
}
