//! Param-edit overlay: a centered modal rendered from [`EditParamsState`].
//!
//! The overlay is drawn on top of the normal layout whenever the app is in
//! [`Mode::EditParams`]. Each schema field gets a label row and an editor
//! row (or block for RuleList). Focused fields are highlighted in cyan;
//! inline errors appear immediately below the field they belong to. A
//! form-level error (from pipe structural validation) appears just above the
//! footer hint line.

use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};

use ripe_core::params::FieldKind;

use crate::app::{App, EditParamsState, FieldEditor, Mode};
use crate::ui::centered_rect;
use crate::ui::theme::Theme;

/// Render the param-edit overlay when `app.mode == EditParams`.
/// Takes `app: &App` (read-only) because only the canvas auto-mutates scroll.
pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let Mode::EditParams(node_id) = app.mode else {
        return;
    };
    let Some(state) = &app.edit_state else {
        return;
    };
    let theme = app.theme;

    // Overlay: roughly 60 % wide and 70 % tall, at least 40×10.
    let ow = ((area.width as u32 * 6 / 10) as u16)
        .max(40)
        .min(area.width);
    let oh = ((area.height as u32 * 7 / 10) as u16)
        .max(10)
        .min(area.height);
    let overlay = centered_rect(ow, oh, area);

    frame.render_widget(Clear, overlay);

    // Title shows the canvas badge, not the internal id — it must name the
    // number the user sees on the box.
    let badge = crate::ui::layout::Layout::compute(&app.pipe).badge(node_id);
    let title = app
        .pipe
        .node(node_id)
        .map(|n| match badge {
            Some(b) => format!(" node #{b} ({}) ", n.kind),
            None => format!(" node {} ({}) ", n.id, n.kind),
        })
        .unwrap_or_else(|| " params ".to_string());

    let block = Block::bordered()
        .border_type(BorderType::Double)
        .border_style(Style::new().fg(theme.accent))
        .title(title);
    let inner = block.inner(overlay);
    frame.render_widget(block, overlay);

    if inner.height == 0 {
        return;
    }

    // Footer hint — always on the last inner row. Enter means "apply" in
    // single-line fields but "new line" in the multi-line rule list, so the
    // hint follows the focused field.
    let multiline_focused = matches!(
        state.editors.get(state.focused),
        Some(crate::app::FieldEditor::RuleList(_))
    );
    let footer_y = inner.y + inner.height.saturating_sub(1);
    render_footer(
        frame,
        Rect {
            x: inner.x,
            y: footer_y,
            width: inner.width,
            height: 1,
        },
        multiline_focused,
        &theme,
    );

    // Form error — one row above the footer (when present).
    let form_err_rows = if state.form_error.is_some() { 1u16 } else { 0 };
    if let Some(err) = &state.form_error {
        let y = footer_y.saturating_sub(1);
        if y >= inner.y {
            render_form_error(
                frame,
                Rect {
                    x: inner.x,
                    y,
                    width: inner.width,
                    height: 1,
                },
                err,
                &theme,
            );
        }
    }

    // Fields fill the remaining space above footer (and form error).
    let fields_height = inner.height.saturating_sub(1 + form_err_rows);
    if fields_height == 0 {
        return;
    }
    let fields_rect = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: fields_height,
    };

    if state.editors.is_empty() {
        // No schema fields — show a placeholder.
        let mid = fields_rect.y + fields_rect.height / 2;
        let r = Rect {
            x: fields_rect.x,
            y: mid,
            width: fields_rect.width,
            height: 1,
        };
        frame.render_widget(
            Paragraph::new("no params")
                .style(Style::new().fg(theme.text_faint))
                .alignment(Alignment::Center),
            r,
        );
        return;
    }

    render_fields(frame, fields_rect, state, &theme);
}

// --- sub-renderers -------------------------------------------------------

fn render_footer(frame: &mut Frame, rect: Rect, multiline_focused: bool, theme: &Theme) {
    let key = Style::new().fg(theme.warn).add_modifier(Modifier::BOLD);
    let mut spans = vec![
        Span::styled(
            if multiline_focused {
                "Ctrl-s"
            } else {
                "Enter/Ctrl-s"
            },
            key,
        ),
        Span::raw(" apply   "),
    ];
    if multiline_focused {
        spans.push(Span::styled("Enter", key));
        spans.push(Span::raw(" new line   "));
    }
    spans.extend([
        Span::styled("Esc", key),
        Span::raw(" cancel   "),
        Span::styled("↑/↓", key),
        Span::raw(" field"),
    ]);
    frame.render_widget(Paragraph::new(Line::from(spans)), rect);
}

fn render_form_error(frame: &mut Frame, rect: Rect, err: &str, theme: &Theme) {
    let line = Line::from(vec![
        Span::styled(
            "⚠ ",
            Style::new().fg(theme.err).add_modifier(Modifier::BOLD),
        ),
        Span::styled(err.to_string(), Style::new().fg(theme.err)),
    ]);
    frame.render_widget(Paragraph::new(line), rect);
}

/// Render the field list inside `area`, top-to-bottom. Stops when the area
/// is exhausted so the footer is never overwritten.
fn render_fields(frame: &mut Frame, area: Rect, state: &EditParamsState, theme: &Theme) {
    let mut y = area.y;
    let bottom = area.y + area.height;

    for (i, (field, editor)) in state
        .schema
        .fields
        .iter()
        .zip(state.editors.iter())
        .enumerate()
    {
        if y >= bottom {
            break;
        }

        let is_focused = i == state.focused;
        let error: Option<&str> = state.field_errors.get(i).and_then(|e| e.as_deref());

        // --- Label row ---------------------------------------------------
        let label_style = if is_focused {
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(theme.text_faint)
        };
        // Add a kind hint in brackets so the type is visible at a glance.
        let kind_tag = match &field.kind {
            FieldKind::Text => "text",
            FieldKind::Url => "url",
            FieldKind::Number => "number",
            FieldKind::Bool => "bool",
            FieldKind::Enum(_) => "enum",
            FieldKind::FieldName => "field",
            FieldKind::RuleList => "rules",
        };
        let label = format!("{} [{}]", field.label, kind_tag);
        if y < bottom {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(label, label_style))),
                Rect {
                    x: area.x,
                    y,
                    width: area.width,
                    height: 1,
                },
            );
            y += 1;
        }

        // --- Editor row(s) -----------------------------------------------
        if y < bottom {
            let editor_rows: u16 = match editor {
                FieldEditor::RuleList(_) => 3,
                _ => 1,
            };
            let avail = editor_rows.min(bottom.saturating_sub(y));
            let editor_rect = Rect {
                x: area.x + 2, // slight indent
                y,
                width: area.width.saturating_sub(2),
                height: avail,
            };
            render_field_editor(frame, editor_rect, editor, is_focused, theme);
            y += avail;
        }

        // --- Inline error row --------------------------------------------
        if let Some(err) = error
            && y < bottom
        {
            let err_line = Line::from(vec![
                Span::styled("  ⚠ ", Style::new().fg(theme.err)),
                Span::styled(err.to_string(), Style::new().fg(theme.err)),
            ]);
            frame.render_widget(
                Paragraph::new(err_line),
                Rect {
                    x: area.x,
                    y,
                    width: area.width,
                    height: 1,
                },
            );
            y += 1;
        }

        // --- Spacer row --------------------------------------------------
        y += 1;
    }
}

/// Draw one field editor widget into `rect`.
fn render_field_editor(
    frame: &mut Frame,
    rect: Rect,
    editor: &FieldEditor,
    focused: bool,
    theme: &Theme,
) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }
    let focus_style = Style::new().fg(theme.text);
    let unfocus_style = Style::new().fg(theme.text_faint);

    match editor {
        FieldEditor::Text(ta) | FieldEditor::RuleList(ta) => {
            // Pass a reference to the textarea; tui-textarea 0.7 implements
            // Widget for &TextArea so no .widget() call is needed.
            frame.render_widget(ta, rect);
        }
        FieldEditor::Bool(b) => {
            let symbol = if *b { "[x]" } else { "[ ]" };
            let style = if focused { focus_style } else { unfocus_style };
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(symbol.to_string(), style))),
                rect,
            );
        }
        FieldEditor::Enum { variants, idx } => {
            let current = variants.get(*idx).copied().unwrap_or("");
            // Show all variants with current highlighted: "permit | [block] | log"
            let parts: Vec<Span> = variants
                .iter()
                .enumerate()
                .flat_map(|(j, &v)| {
                    let sep: Option<Span> = if j > 0 { Some(Span::raw(" | ")) } else { None };
                    let span = if j == *idx {
                        Span::styled(
                            format!("[{v}]"),
                            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
                        )
                    } else if focused {
                        Span::styled(v.to_string(), focus_style)
                    } else {
                        Span::styled(v.to_string(), unfocus_style)
                    };
                    sep.into_iter().chain(std::iter::once(span))
                })
                .collect();
            let _ = current; // suppress warning; used via idx
            frame.render_widget(Paragraph::new(Line::from(parts)), rect);
        }
    }
}
