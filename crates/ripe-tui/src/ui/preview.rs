//! Right pane: the output preview. In M8 the FEED / ITEMS / RAW tab bar and
//! the auto-refresh footer are drawn but inert; the tabs get content, and the
//! selection becomes live, in M13.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Tabs};

use crate::app::App;
use crate::ui::pane_block;

pub(crate) fn render(frame: &mut Frame, area: Rect, _app: &App, focused: bool) {
    let block = pane_block("Preview", focused);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let rows = Layout::vertical([
        Constraint::Length(1), // tab bar
        Constraint::Min(0),    // body
        Constraint::Length(1), // footer
    ])
    .split(inner);

    let tabs = Tabs::new(vec!["FEED", "ITEMS", "RAW"])
        .select(0)
        .highlight_style(Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .divider("  ");
    frame.render_widget(tabs, rows[0]);

    let body = Paragraph::new(vec![
        Line::raw(""),
        Line::from(Span::styled(
            "M13: preview lands here",
            Style::new().fg(Color::DarkGray),
        )),
    ])
    .alignment(Alignment::Center);
    frame.render_widget(body, rows[1]);

    let footer = Paragraph::new(Line::from(Span::styled(
        "Auto refresh: on",
        Style::new().fg(Color::DarkGray),
    )))
    .alignment(Alignment::Right);
    frame.render_widget(footer, rows[2]);
}
