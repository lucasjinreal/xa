//! TUI color themes: dark (default) and light, with auto detection.
//!
//! UI chrome keeps a warm orange brand accent. Assistant markdown follows a
//! Grok/Codex-style minimal palette (soft body gray, blue/purple headings,
//! restrained syntax, low-sat diffs). Light mode mirrors the same roles with
//! darker inks on pale surfaces.
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

    /// Dark theme: Grok/Codex-minimal markdown + warm orange UI chrome.
    ///
    /// Core assistant colors (~8): body `#C8C8C8`, blue `#7AA2F7`, purple
    /// `#BB9AF7`, mid-gray `#707070`, cyan `#3A95AB`, gold `#E0AF68`,
    /// code bg `#1C1C1C`, plus low-sat green/red for diffs.
    pub fn dark() -> Self {
        // Core markdown / syntax tokens.
        let body = Color::Rgb(0xC8, 0xC8, 0xC8); // #C8C8C8
        let blue = Color::Rgb(0x7A, 0xA2, 0xF7); // #7AA2F7
        let purple = Color::Rgb(0xBB, 0x9A, 0xF7); // #BB9AF7
        let mid = Color::Rgb(0x70, 0x70, 0x70); // #707070
        let cyan = Color::Rgb(0x3A, 0x95, 0xAB); // #3A95AB
        let gold = Color::Rgb(0xE0, 0xAF, 0x68); // #E0AF68
        let code_bg = Color::Rgb(0x1C, 0x1C, 0x1C); // #1C1C1C
        let code_plain = Color::Rgb(0xA8, 0xA8, 0xA8); // #A8A8A8
        let accent = Color::Rgb(217, 119, 87); // UI brand (not markdown)

        Self {
            mode: ColorMode::Dark,
            accent,
            accent_dim: Color::Rgb(180, 100, 70),
            accent_bright: Color::Rgb(240, 150, 110),
            text: body,
            text_dim: mid,
            text_hint: mid,
            bg: Color::Rgb(22, 22, 22),
            surface: Color::Rgb(36, 36, 36),
            code_bg,
            code_text: code_plain,
            user_bg: Color::Rgb(34, 34, 34),
            input_bg: Color::Rgb(30, 30, 30),
            footer: mid,
            user_lead: Color::Rgb(150, 150, 150),
            input_lead: Color::Rgb(240, 240, 240),
            select_bg: Color::Rgb(70, 48, 28),
            border: Color::Rgb(72, 72, 72),
            field_bg: Color::Rgb(28, 28, 28),
            shimmer_peak: Color::Rgb(255, 255, 255),
            success: Color::Rgb(0x9E, 0xCE, 0x6A),
            error: Color::Rgb(0xF7, 0x76, 0x8E),
            warning: gold,
            // Diff — low saturation (Edit tool).
            diff_add: Color::Rgb(0x9E, 0xCE, 0x6A), // #9ECE6A
            diff_del: Color::Rgb(0xF7, 0x76, 0x8E), // #F7768E
            diff_meta: mid,
            diff_add_bg: Color::Rgb(0x06, 0x38, 0x06), // #063806
            diff_del_bg: Color::Rgb(0x42, 0x0E, 0x14), // #420E14
            // Markdown
            md_text: body,
            md_heading1: blue,
            md_heading2: purple,
            md_heading3: purple,
            md_muted: mid, // list bullets / rules / quotes
            md_inline_code: cyan,
            md_link: cyan,
            md_info: cyan,
            // Syntax — only a few semantic hues; everything else = body.
            syn_comment: mid,
            syn_keyword: purple,
            syn_string: gold,
            syn_string_escape: gold,
            syn_number: gold,
            syn_constant: gold,
            syn_function: blue,
            syn_type: code_plain,
            syn_variable: code_plain,
            syn_property: code_plain,
            syn_operator: code_plain,
            syn_punctuation: code_plain,
            syn_attribute: cyan, // macros / attributes
            syn_tag: cyan,
            syn_label: cyan,
            syn_error: Color::Rgb(0xF7, 0x76, 0x8E),
        }
    }

    /// Light theme: same role mapping, darker inks on pale surfaces.
    pub fn light() -> Self {
        let body = Color::Rgb(0x2A, 0x2A, 0x2A);
        let blue = Color::Rgb(0x2E, 0x5C, 0xC7);
        let purple = Color::Rgb(0x6B, 0x4F, 0xC7);
        let mid = Color::Rgb(0x70, 0x70, 0x70);
        let cyan = Color::Rgb(0x1F, 0x7A, 0x8C);
        let gold = Color::Rgb(0xB0, 0x7D, 0x28);
        let code_bg = Color::Rgb(0xEE, 0xEE, 0xEE);
        let accent = Color::Rgb(196, 95, 58);

        Self {
            mode: ColorMode::Light,
            accent,
            accent_dim: Color::Rgb(165, 85, 55),
            accent_bright: Color::Rgb(220, 120, 80),
            text: body,
            text_dim: mid,
            text_hint: mid,
            bg: Color::Rgb(248, 248, 248),
            surface: Color::Rgb(240, 240, 240),
            code_bg,
            code_text: body,
            user_bg: Color::Rgb(234, 234, 234),
            input_bg: Color::Rgb(242, 242, 242),
            footer: mid,
            user_lead: Color::Rgb(110, 110, 110),
            input_lead: Color::Rgb(40, 40, 40),
            select_bg: Color::Rgb(255, 228, 208),
            border: Color::Rgb(190, 190, 190),
            field_bg: Color::Rgb(255, 255, 255),
            shimmer_peak: accent,
            success: Color::Rgb(0x3D, 0x8B, 0x40),
            error: Color::Rgb(0xC4, 0x3E, 0x55),
            warning: gold,
            diff_add: Color::Rgb(0x3D, 0x8B, 0x40),
            diff_del: Color::Rgb(0xC4, 0x3E, 0x55),
            diff_meta: mid,
            diff_add_bg: Color::Rgb(0xE0, 0xF0, 0xE0),
            diff_del_bg: Color::Rgb(0xF8, 0xE4, 0xE6),
            md_text: body,
            md_heading1: blue,
            md_heading2: purple,
            md_heading3: purple,
            md_muted: mid,
            md_inline_code: cyan,
            md_link: cyan,
            md_info: cyan,
            syn_comment: mid,
            syn_keyword: purple,
            syn_string: gold,
            syn_string_escape: gold,
            syn_number: gold,
            syn_constant: gold,
            syn_function: blue,
            syn_type: body,
            syn_variable: body,
            syn_property: body,
            syn_operator: body,
            syn_punctuation: body,
            syn_attribute: cyan,
            syn_tag: cyan,
            syn_label: cyan,
            syn_error: Color::Rgb(0xC4, 0x3E, 0x55),
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
        let incomplete = b"\x1b]11;rgb:1212/1212/1212\x1b";
        assert_eq!(detect::parse_osc11_response(incomplete), None);
        // 2-digit hex components
        let s3 = b"\x1b]11;rgb:ab/cd/ef\x07";
        let (r, g, b) = detect::parse_osc11_response(s3).unwrap();
        assert!(r > 100 && g > 100 && b > 100);
    }
}
