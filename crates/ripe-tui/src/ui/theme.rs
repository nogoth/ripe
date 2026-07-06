//! Color themes. Every color the view uses comes off a [`Theme`] so a config
//! switch restyles the whole editor; no render fn names a `Color::` directly.
//!
//! Both palettes stick to ANSI names where possible — the terminal's own
//! palette then keeps hues readable — and only reach for RGB where ANSI has
//! no usable slot (the light theme's amber, the zebra divider).

use ratatui::style::Color;

/// Named color roles. Fields are grouped by what they style, not by hue, so
/// a new theme only has to answer "what should X look like".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    /// Focus borders, the selected node, the active preview tab.
    pub accent: Color,
    /// Unfocused pane borders.
    pub border_dim: Color,
    /// Emphasized foreground (prompt text, focused field).
    pub text: Color,
    /// Secondary text (param summaries, help descriptions, raw output).
    pub text_dim: Color,
    /// Tertiary text (placeholders, footers, disabled hints).
    pub text_faint: Color,
    /// Success statuses (`✓ n items`, Normal mode tag).
    pub ok: Color,
    /// Errors.
    pub err: Color,
    /// Warnings, hints, spinners, palette letters, Insert mode tag.
    pub warn: Color,
    /// The Params mode tag and other modal accents.
    pub modal: Color,
    /// Top-bar menu chip.
    pub menu_bg: Color,
    pub menu_fg: Color,
    /// The card divider in the preview's ITEMS tab.
    pub divider: Color,
}

impl Theme {
    pub fn dark() -> Self {
        Theme {
            accent: Color::Cyan,
            border_dim: Color::DarkGray,
            text: Color::White,
            text_dim: Color::Gray,
            text_faint: Color::DarkGray,
            ok: Color::Green,
            err: Color::Red,
            warn: Color::Yellow,
            modal: Color::Magenta,
            menu_bg: Color::Blue,
            menu_fg: Color::White,
            divider: Color::Rgb(40, 40, 40),
        }
    }

    pub fn light() -> Self {
        Theme {
            accent: Color::Blue,
            border_dim: Color::Gray,
            text: Color::Black,
            text_dim: Color::DarkGray,
            text_faint: Color::Gray,
            ok: Color::Green,
            err: Color::Red,
            // ANSI yellow is unreadable on light backgrounds; use an amber.
            warn: Color::Rgb(150, 90, 0),
            modal: Color::Magenta,
            menu_bg: Color::Blue,
            menu_fg: Color::White,
            divider: Color::Rgb(220, 220, 220),
        }
    }

    /// Theme by config name; `None` for anything unrecognized (the caller
    /// warns and keeps the default).
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "dark" => Some(Theme::dark()),
            "light" => Some(Theme::light()),
            _ => None,
        }
    }

    /// Node border/title tint by module kind, echoing the mockup's grouping.
    /// Shared across themes: these are ANSI hues the terminal palette adapts.
    pub fn kind_color(&self, kind: &str) -> Color {
        match kind {
            "fetch_feed" | "fetch_json" | "fetch_csv" | "output" => Color::Blue,
            "regex" | "sort" => self.warn,
            "filter" | "transform" | "union" | "unique" => Color::Magenta,
            "limit" | "tail" | "reverse" => Color::Green,
            _ => self.text_dim,
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Theme::dark()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_name_resolves_known_themes_only() {
        assert_eq!(Theme::from_name("dark"), Some(Theme::dark()));
        assert_eq!(Theme::from_name("light"), Some(Theme::light()));
        assert_eq!(Theme::from_name("solarized"), None);
        assert_eq!(Theme::from_name(""), None);
    }

    #[test]
    fn themes_differ_where_it_matters() {
        let (d, l) = (Theme::dark(), Theme::light());
        assert_ne!(d.text, l.text);
        assert_ne!(d.accent, l.accent);
        assert_ne!(d.warn, l.warn, "light must not use ANSI yellow");
    }
}
