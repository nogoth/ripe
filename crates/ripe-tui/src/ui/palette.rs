//! Left sidebar: the module catalog, grouped NODES / SOURCES / SINKS.
//!
//! Grouping is derived from the live [`Registry`], not a hardcoded list, so
//! the palette can never disagree with what the engine can actually build:
//! a module with no inputs is a source, the `output` kind is a sink, and
//! everything else is an operator node. Node rows show their insert letter
//! (PLAN.md `a <letter>`); sources and sinks are letter-less until M10 wires
//! their insert flow.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use ripe_core::Registry;

use crate::app::App;
use crate::ui::pane_block;

/// The insert letter for a node kind, per the mockup / PLAN.md keymap. `None`
/// for kinds reached another way (sources, sinks) — those list without a
/// letter for now.
pub fn insert_letter(kind: &str) -> Option<char> {
    Some(match kind {
        "filter" => 't',
        "regex" => 'r',
        "transform" => 'm',
        "sort" => 's',
        "union" => 'u',
        "unique" => 'q',
        "limit" => 'l',
        "output" => 'o',
        _ => return None,
    })
}

#[derive(Default)]
struct Groups {
    nodes: Vec<&'static str>,
    sources: Vec<&'static str>,
    sinks: Vec<&'static str>,
}

/// Split the registry's kinds into the three palette groups. `kinds()` is
/// already sorted, so each group keeps a stable order.
fn grouped(registry: &Registry) -> Groups {
    let mut groups = Groups::default();
    for kind in registry.kinds() {
        let module = registry
            .get(kind)
            .expect("kinds() only lists registered kinds");
        if module.inputs().is_empty() {
            groups.sources.push(kind);
        } else if kind == "output" {
            groups.sinks.push(kind);
        } else {
            groups.nodes.push(kind);
        }
    }
    groups
}

pub(crate) fn render(frame: &mut Frame, area: Rect, app: &App, focused: bool) {
    let block = pane_block("Palette", focused);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let width = inner.width as usize;
    let groups = grouped(&app.registry);
    let mut lines = Vec::new();
    push_group(&mut lines, "NODES", &groups.nodes, width, true);
    lines.push(Line::raw(""));
    push_group(&mut lines, "SOURCES", &groups.sources, width, false);
    lines.push(Line::raw(""));
    push_group(&mut lines, "SINKS", &groups.sinks, width, false);

    frame.render_widget(Paragraph::new(lines), inner);
}

fn push_group(
    lines: &mut Vec<Line<'static>>,
    header: &str,
    kinds: &[&'static str],
    width: usize,
    with_letters: bool,
) {
    lines.push(Line::from(Span::styled(
        header.to_string(),
        Style::new().fg(Color::Gray).add_modifier(Modifier::BOLD),
    )));
    for &kind in kinds {
        let letter = with_letters.then(|| insert_letter(kind)).flatten();
        lines.push(entry_line(kind, letter, width));
    }
}

/// One catalog row: kind on the left, insert letter (when it has one) pushed
/// to the right edge, mockup-style.
fn entry_line(kind: &str, letter: Option<char>, width: usize) -> Line<'static> {
    let name = format!("  {kind}");
    match letter {
        Some(letter) => {
            let pad = width.saturating_sub(name.chars().count() + 1);
            Line::from(vec![
                Span::raw(name),
                Span::raw(" ".repeat(pad)),
                Span::styled(letter.to_string(), Style::new().fg(Color::Yellow)),
            ])
        }
        None => Line::from(name),
    }
}
