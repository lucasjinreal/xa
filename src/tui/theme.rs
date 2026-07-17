//! TUI color themes: dark (default) and light, with auto detection.
//!
//! Surfaces stay neutral gray; brand emphasis is warm orange. Markdown body
//! text is a soft warm gray (never pure white on dark). Syntax highlighting
//! uses a calm, readable suite similar to GrokNight’s dark code blocks, or a
//! deeper GitHub-light-inspired suite on light terminals.
//!
//! Call [`init`] (or [`init_from_preference`]) once before any TUI draw. After
//! that, every widget reads colors via [`t`].

use std::sync::OnceLock;

use ratatui::style::Color;
use ratatui_markdown::theme::{CodeColors, ThemeConfig};

mod detect;

/// Resolved appearance after preference + detection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorMode {
    Dark,
    Light,
}

/// User preference: force a mode, or detect from the terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemePreference {
    Auto,
    Dark,
    Light,
}

impl ThemePreference {
    /// Parse `auto` / `dark` / `light` (case-insensitive). Unknown → `None`.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "dark" => Some(Self::Dark),
            "light" => Some(Self::Light),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }
}

/// Full semantic palette for one appearance.
///
/// Not every field is referenced by every widget; unused tokens stay available
/// for consistent future chrome (success chips, dim accents, etc.).
#[derive(Clone, Copy, Debug)]
#[allow(dead_code)]
pub struct Theme {
    pub mode: ColorMode,

    pub accent: Color,
    pub accent_dim: Color,
    pub accent_bright: Color,

    pub text: Color,
    pub text_dim: Color,
    pub text_hint: Color,

    pub bg: Color,
    pub surface: Color,
    pub code_bg: Color,
    pub code_text: Color,
    pub user_bg: Color,
    pub input_bg: Color,
    pub footer: Color,
    pub user_lead: Color,
    pub input_lead: Color,
    pub select_bg: Color,
    pub border: Color,

    /// Wizard / modal text-field strip (slightly inset from panel).
    pub field_bg: Color,
    /// Shimmer highlight peak (status / streaming labels).
    pub shimmer_peak: Color,

    pub success: Color,
    pub error: Color,
    pub warning: Color,

    pub diff_add: Color,
    pub diff_del: Color,
    pub diff_meta: Color,
    pub diff_add_bg: Color,
    pub diff_del_bg: Color,

    pub md_text: Color,
    pub md_heading1: Color,
    pub md_heading2: Color,
    pub md_heading3: Color,
    pub md_muted: Color,
    pub md_inline_code: Color,
    pub md_link: Color,
    pub md_info: Color,

    pub syn_comment: Color,
    pub syn_keyword: Color,
    pub syn_string: Color,
    pub syn_string_escape: Color,
    pub syn_number: Color,
    pub syn_constant: Color,
    pub syn_function: Color,
    pub syn_type: Color,
    pub syn_variable: Color,
    pub syn_property: Color,
    pub syn_operator: Color,
    pub syn_punctuation: Color,
    pub syn_attribute: Color,
    pub syn_tag: Color,
    pub syn_label: Color,
    pub syn_error: Color,
}

impl Theme {
    pub fn for_mode(mode: ColorMode) -> Self {
        match mode {
            ColorMode::Dark => Self::dark(),
            ColorMode::Light => Self::light(),
        }
    }

    /// Gray + orange soft dark (historical default).
    pub fn dark() -> Self {
        let accent = Color::Rgb(217, 119, 87);
        Self {
            mode: ColorMode::Dark,
            accent,
            accent_dim: Color::Rgb(180, 100, 70),
            accent_bright: Color::Rgb(240, 150, 110),
            text: Color::Rgb(225, 225, 225),
            text_dim: Color::Rgb(145, 145, 145),
            text_hint: Color::Rgb(110, 110, 110),
            bg: Color::Rgb(22, 22, 22),
            surface: Color::Rgb(36, 36, 36),
            code_bg: Color::Rgb(28, 28, 30),
            code_text: Color::Rgb(168, 166, 158),
            user_bg: Color::Rgb(34, 34, 34),
            input_bg: Color::Rgb(30, 30, 30),
            footer: Color::Rgb(130, 130, 130),
            user_lead: Color::Rgb(150, 150, 150),
            input_lead: Color::Rgb(240, 240, 240),
            select_bg: Color::Rgb(70, 48, 28),
            border: Color::Rgb(72, 72, 72),
            field_bg: Color::Rgb(28, 28, 28),
            shimmer_peak: Color::Rgb(255, 255, 255),
            success: Color::Rgb(120, 185, 105),
            error: Color::Rgb(220, 95, 75),
            warning: Color::Rgb(220, 170, 70),
            diff_add: Color::Rgb(100, 185, 120),
            diff_del: Color::Rgb(220, 115, 105),
            diff_meta: Color::Rgb(120, 120, 120),
            diff_add_bg: Color::Rgb(30, 50, 35),
            diff_del_bg: Color::Rgb(55, 30, 28),
            md_text: Color::Rgb(225, 225, 225),
            md_heading1: accent,
            md_heading2: Color::Rgb(214, 198, 178),
            md_heading3: Color::Rgb(168, 148, 128),
            md_muted: Color::Rgb(118, 114, 108),
            md_inline_code: Color::Rgb(214, 176, 110),
            md_link: Color::Rgb(224, 140, 100),
            md_info: Color::Rgb(130, 165, 155),
            syn_comment: Color::Rgb(110, 108, 102),
            syn_keyword: Color::Rgb(214, 130, 110),
            syn_string: Color::Rgb(152, 180, 130),
            syn_string_escape: Color::Rgb(178, 198, 150),
            syn_number: Color::Rgb(210, 170, 110),
            syn_constant: Color::Rgb(200, 155, 130),
            syn_function: Color::Rgb(130, 175, 190),
            syn_type: Color::Rgb(145, 175, 160),
            syn_variable: Color::Rgb(168, 166, 158),
            syn_property: Color::Rgb(160, 170, 185),
            syn_operator: Color::Rgb(150, 145, 140),
            syn_punctuation: Color::Rgb(120, 118, 114),
            syn_attribute: Color::Rgb(200, 170, 120),
            syn_tag: Color::Rgb(130, 175, 190),
            syn_label: Color::Rgb(200, 140, 130),
            syn_error: Color::Rgb(220, 100, 90),
        }
    }

    /// Soft light: dark text on near-white surfaces, deeper accent for contrast.
    pub fn light() -> Self {
        let accent = Color::Rgb(196, 95, 58);
        Self {
            mode: ColorMode::Light,
            accent,
            accent_dim: Color::Rgb(165, 85, 55),
            accent_bright: Color::Rgb(220, 120, 80),
            text: Color::Rgb(32, 32, 32),
            text_dim: Color::Rgb(100, 100, 100),
            text_hint: Color::Rgb(130, 130, 130),
            bg: Color::Rgb(248, 248, 248),
            surface: Color::Rgb(240, 240, 240),
            code_bg: Color::Rgb(236, 236, 238),
            code_text: Color::Rgb(55, 54, 50),
            user_bg: Color::Rgb(234, 234, 234),
            input_bg: Color::Rgb(242, 242, 242),
            footer: Color::Rgb(110, 110, 110),
            user_lead: Color::Rgb(110, 110, 110),
            input_lead: Color::Rgb(40, 40, 40),
            select_bg: Color::Rgb(255, 228, 208),
            border: Color::Rgb(190, 190, 190),
            field_bg: Color::Rgb(255, 255, 255),
            shimmer_peak: Color::Rgb(196, 95, 58),
            success: Color::Rgb(40, 140, 60),
            error: Color::Rgb(190, 55, 45),
            warning: Color::Rgb(170, 120, 20),
            diff_add: Color::Rgb(30, 130, 60),
            diff_del: Color::Rgb(180, 50, 45),
            diff_meta: Color::Rgb(120, 120, 120),
            diff_add_bg: Color::Rgb(220, 245, 225),
            diff_del_bg: Color::Rgb(255, 230, 228),
            md_text: Color::Rgb(32, 32, 32),
            md_heading1: accent,
            md_heading2: Color::Rgb(90, 70, 50),
            md_heading3: Color::Rgb(110, 90, 70),
            md_muted: Color::Rgb(130, 125, 120),
            md_inline_code: Color::Rgb(150, 95, 25),
            md_link: Color::Rgb(185, 85, 50),
            md_info: Color::Rgb(40, 110, 100),
            // Deeper tokens for light code blocks (GitHub-light inspired).
            syn_comment: Color::Rgb(110, 115, 120),
            syn_keyword: Color::Rgb(175, 55, 45),
            syn_string: Color::Rgb(40, 125, 55),
            syn_string_escape: Color::Rgb(50, 140, 70),
            syn_number: Color::Rgb(150, 95, 25),
            syn_constant: Color::Rgb(160, 80, 50),
            syn_function: Color::Rgb(30, 110, 150),
            syn_type: Color::Rgb(40, 120, 105),
            syn_variable: Color::Rgb(55, 54, 50),
            syn_property: Color::Rgb(50, 90, 140),
            syn_operator: Color::Rgb(90, 90, 90),
            syn_punctuation: Color::Rgb(120, 120, 120),
            syn_attribute: Color::Rgb(140, 100, 30),
            syn_tag: Color::Rgb(30, 110, 150),
            syn_label: Color::Rgb(160, 70, 60),
            syn_error: Color::Rgb(190, 40, 40),
        }
    }

    pub fn code_syntax_colors(&self) -> CodeColors {
        CodeColors {
            comment: self.syn_comment,
            keyword: self.syn_keyword,
            string: self.syn_string,
            string_escape: self.syn_string_escape,
            number: self.syn_number,
            constant: self.syn_constant,
            function: self.syn_function,
            r#type: self.syn_type,
            variable: self.syn_variable,
            property: self.syn_property,
            operator: self.syn_operator,
            punctuation: self.syn_punctuation,
            attribute: self.syn_attribute,
            tag: self.syn_tag,
            label: self.syn_label,
            error: self.syn_error,
        }
    }

    /// Full `ratatui-markdown` theme for assistant markdown rendering.
    pub fn markdown_theme(&self) -> ThemeConfig {
        ThemeConfig {
            text_color: self.md_text,
            muted_text_color: self.md_muted,
            primary_color: self.md_link,
            popup_selected_background: self.select_bg,
            border_color: self.border,
            focused_border_color: self.accent,
            secondary_color: self.md_heading3,
            info_color: self.md_info,
            json_key_color: self.syn_property,
            json_string_color: self.syn_string,
            json_number_color: self.syn_number,
            json_bool_color: self.syn_keyword,
            json_null_color: self.syn_comment,
            accent_yellow: self.md_inline_code,
            code_colors: self.code_syntax_colors(),
            ..ThemeConfig::default()
        }
    }

    /// RGB triple for a `Color::Rgb`, used when driving crossterm directly.
    pub fn rgb(color: Color) -> Option<(u8, u8, u8)> {
        match color {
            Color::Rgb(r, g, b) => Some((r, g, b)),
            _ => None,
        }
    }
}

static THEME: OnceLock<Theme> = OnceLock::new();

/// Install the active theme. Safe to call once at process start; later calls
/// are ignored (the first wins). Prefer [`init_from_preference`].
pub fn init(mode: ColorMode) {
    let _ = THEME.set(Theme::for_mode(mode));
}

/// Resolve `pref` (detecting when `Auto`) and install the theme. Returns the
/// mode that was actually applied.
pub fn init_from_preference(pref: ThemePreference) -> ColorMode {
    let mode = match pref {
        ThemePreference::Dark => ColorMode::Dark,
        ThemePreference::Light => ColorMode::Light,
        ThemePreference::Auto => detect::detect_color_mode(),
    };
    init(mode);
    mode
}

/// Active theme. Defaults to dark if [`init`] was never called.
pub fn t() -> &'static Theme {
    THEME.get_or_init(Theme::dark)
}

/// Syntax colors used by tree-sitter highlighting.
pub fn code_syntax_colors() -> CodeColors {
    t().code_syntax_colors()
}

/// Full `ratatui-markdown` theme for assistant markdown rendering.
pub fn markdown_theme() -> ThemeConfig {
    t().markdown_theme()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preference_parse() {
        assert_eq!(ThemePreference::parse("auto"), Some(ThemePreference::Auto));
        assert_eq!(ThemePreference::parse("DARK"), Some(ThemePreference::Dark));
        assert_eq!(ThemePreference::parse("Light"), Some(ThemePreference::Light));
        assert_eq!(ThemePreference::parse("nope"), None);
    }

    #[test]
    fn light_text_darker_than_bg() {
        let th = Theme::light();
        let (tr, tg, tb) = Theme::rgb(th.text).unwrap();
        let (br, bg, bb) = Theme::rgb(th.bg).unwrap();
        let text_l = (tr as u32 + tg as u32 + tb as u32) / 3;
        let bg_l = (br as u32 + bg as u32 + bb as u32) / 3;
        assert!(text_l < bg_l, "light theme body text should be darker than bg");
    }

    #[test]
    fn dark_text_lighter_than_bg() {
        let th = Theme::dark();
        let (tr, tg, tb) = Theme::rgb(th.text).unwrap();
        let (br, bg, bb) = Theme::rgb(th.bg).unwrap();
        let text_l = (tr as u32 + tg as u32 + tb as u32) / 3;
        let bg_l = (br as u32 + bg as u32 + bb as u32) / 3;
        assert!(text_l > bg_l, "dark theme body text should be lighter than bg");
    }

    #[test]
    fn luminance_threshold() {
        assert_eq!(
            detect::mode_from_rgb(255, 255, 255),
            ColorMode::Light
        );
        assert_eq!(detect::mode_from_rgb(0, 0, 0), ColorMode::Dark);
        assert_eq!(detect::mode_from_rgb(30, 30, 30), ColorMode::Dark);
        assert_eq!(detect::mode_from_rgb(240, 240, 240), ColorMode::Light);
    }

    #[test]
    fn colorfgbg_heuristic() {
        assert_eq!(
            detect::mode_from_colorfgbg("15;0"),
            Some(ColorMode::Dark)
        );
        assert_eq!(
            detect::mode_from_colorfgbg("0;15"),
            Some(ColorMode::Light)
        );
        assert_eq!(detect::mode_from_colorfgbg("bogus"), None);
    }

    #[test]
    fn osc11_parse() {
        let s = "\x1b]11;rgb:ffff/ffff/ffff\x07";
        assert_eq!(
            detect::parse_osc11_response(s.as_bytes()),
            Some((255, 255, 255))
        );
        let s2 = "\x1b]11;rgb:0000/0000/0000\x1b\\";
        assert_eq!(
            detect::parse_osc11_response(s2.as_bytes()),
            Some((0, 0, 0))
        );
        // 2-digit hex components
        let s3 = b"\x1b]11;rgb:ab/cd/ef\x07";
        let (r, g, b) = detect::parse_osc11_response(s3).unwrap();
        assert!(r > 100 && g > 100 && b > 100);
    }
}
