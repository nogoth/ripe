//! Left sidebar: the module catalog, grouped NODES / SOURCES / SINKS.
//!
//! Grouping is derived from the live [`Registry`], not a hardcoded list, so
//! the palette can never disagree with what the engine can actually build:
//! a module with no inputs is a source, the `output` kind is a sink, and
//! everything else is an operator node. Every row shows its insert letter
//! (`a <letter>`) so the user does not have to memorise the keymap.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use ripe_core::Registry;

use crate::app::App;
use crate::ui::pane_block;
use crate::ui::theme::Theme;

/// The insert letter for a node kind, per PLAN.md keybindings table.
///
/// Letters are assigned to avoid collisions with:
/// - global keys: Tab, ?, Esc, Ctrl-C/S/O
/// - canvas normal-mode keys: a (leader), d, x, c, j, k, h, l, q, 1-9
///
/// Assignments:
/// | letter | kind        |
/// |--------|-------------|
/// | t      | filter      |
/// | r      | regex       |
/// | m      | transform   |
/// | s      | sort        |
/// | u      | union       |
/// | q      | unique      | (collision with quit is safe: a+q is the leader sequence)
/// | l      | limit       |
/// | o      | output      |
/// | f      | fetch_feed  |
/// | j      | fetch_json  | (collision with down is safe: only active after `a`)
/// | v      | fetch_csv   |
/// | i      | tail        |
/// | e      | reverse     |
pub fn insert_letter(kind: &str) -> Option<char> {
    Some(match kind {
        // Operators (shown in NODES group)
        "filter" => 't',
        "regex" => 'r',
        "transform" => 'm',
        "sort" => 's',
        "union" => 'u',
        "unique" => 'q',
        "limit" => 'l',
        "tail" => 'i',
        "reverse" => 'e',
        // Sources
        "fetch_feed" => 'f',
        "fetch_json" => 'j',
        "fetch_csv" => 'v',
        // Sinks
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

/// The module kind rendered on palette row `row` (0-based, relative to the
/// pane's inner top). Mirrors the row structure `render` produces — header,
/// entries, blank line, next group — so a mouse click maps a row straight
/// back to an insertable kind. Headers and blanks return `None`.
pub(crate) fn kind_at_row(registry: &Registry, row: usize) -> Option<&'static str> {
    let groups = grouped(registry);
    let mut cursor = 0usize;
    for group in [&groups.nodes, &groups.sources, &groups.sinks] {
        // Header line.
        if row == cursor {
            return None;
        }
        cursor += 1;
        // Entry lines.
        if row < cursor + group.len() {
            return Some(group[row - cursor]);
        }
        cursor += group.len();
        // The blank separator line.
        if row == cursor {
            return None;
        }
        cursor += 1;
    }
    None
}

pub(crate) fn render(frame: &mut Frame, area: Rect, app: &App, focused: bool) {
    let theme = app.theme;
    let block = pane_block("Palette", focused, &theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let width = inner.width as usize;
    let groups = grouped(&app.registry);
    let mut lines = Vec::new();
    // All three groups now show insert letters. Row structure must stay in
    // lockstep with `kind_at_row` above.
    push_group(&mut lines, "NODES", &groups.nodes, width, &theme);
    lines.push(Line::raw(""));
    push_group(&mut lines, "SOURCES", &groups.sources, width, &theme);
    lines.push(Line::raw(""));
    push_group(&mut lines, "SINKS", &groups.sinks, width, &theme);

    frame.render_widget(Paragraph::new(lines), inner);
}

fn push_group(
    lines: &mut Vec<Line<'static>>,
    header: &str,
    kinds: &[&'static str],
    width: usize,
    theme: &Theme,
) {
    lines.push(Line::from(Span::styled(
        header.to_string(),
        Style::new().fg(theme.text_dim).add_modifier(Modifier::BOLD),
    )));
    for &kind in kinds {
        lines.push(entry_line(kind, insert_letter(kind), width, theme));
    }
}

/// One catalog row: kind on the left, insert letter pushed to the right edge.
fn entry_line(kind: &str, letter: Option<char>, width: usize, theme: &Theme) -> Line<'static> {
    let name = format!("  {kind}");
    match letter {
        Some(letter) => {
            let pad = width.saturating_sub(name.chars().count() + 1);
            Line::from(vec![
                Span::raw(name),
                Span::raw(" ".repeat(pad)),
                Span::styled(letter.to_string(), Style::new().fg(theme.warn)),
            ])
        }
        None => Line::from(name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_at_row_mirrors_the_rendered_rows() {
        let registry = Registry::with_builtins();
        let groups = grouped(&registry);

        // Row 0 is the NODES header; row 1 the first operator.
        assert_eq!(kind_at_row(&registry, 0), None);
        assert_eq!(kind_at_row(&registry, 1), Some(groups.nodes[0]));
        // The row after the last operator is the blank separator, then the
        // SOURCES header, then the first source.
        let blank = 1 + groups.nodes.len();
        assert_eq!(kind_at_row(&registry, blank), None);
        assert_eq!(kind_at_row(&registry, blank + 1), None);
        assert_eq!(kind_at_row(&registry, blank + 2), Some(groups.sources[0]));
        // Far past the catalog: nothing.
        assert_eq!(kind_at_row(&registry, 999), None);
    }
}
