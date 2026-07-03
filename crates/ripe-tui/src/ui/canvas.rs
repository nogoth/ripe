//! Center pane: the DAG canvas. A placeholder in M8 — it names the open pipe
//! and reserves the space. Layered auto-layout and node/wire rendering land
//! in M9.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::ui::pane_block;

pub(crate) fn render(frame: &mut Frame, area: Rect, app: &App, focused: bool) {
    let block = pane_block("Canvas", focused);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let name = if app.pipe.name.is_empty() {
        "untitled"
    } else {
        &app.pipe.name
    };
    let lines = vec![
        Line::from(Span::styled(
            format!("pipe: {name}"),
            Style::new().add_modifier(Modifier::BOLD),
        )),
        Line::from(format!("{} node(s)", app.node_count())),
        Line::raw(""),
        Line::from(Span::styled(
            "M9: canvas rendering lands here",
            Style::new().fg(Color::DarkGray),
        )),
    ];
    frame.render_widget(Paragraph::new(lines), inner);
}
