//! Gray + orange TUI palette (Grok Build–inspired soft dark).
//!
//! Surfaces stay neutral gray; brand emphasis is warm orange. Markdown body
//! text is a soft warm gray (never pure white). Syntax highlighting uses a
//! calm, readable suite similar to GrokNight’s dark code blocks.

use ratatui::style::Color;
use ratatui_markdown::theme::{CodeColors, ThemeConfig};

/// Brand accent (warm orange).
pub const ACCENT: Color = Color::Rgb(217, 119, 87);
/// Softer orange for secondary emphasis / selected idle states.
pub const ACCENT_DIM: Color = Color::Rgb(180, 100, 70);
/// Bright peak used by shimmer highlights.
pub const ACCENT_BRIGHT: Color = Color::Rgb(240, 150, 110);

/// Primary body text (UI chrome + markdown prose).
pub const TEXT: Color = Color::Rgb(225, 225, 225);
/// Secondary / muted labels.
pub const TEXT_DIM: Color = Color::Rgb(145, 145, 145);
/// Placeholder / tips.
pub const TEXT_HINT: Color = Color::Rgb(110, 110, 110);

/// True neutral grays (R=G=B) — avoid blue-shifted “slate” that reads purple.
pub const BG: Color = Color::Rgb(22, 22, 22);
/// Elevated surface (header box, cards, popups).
#[allow(dead_code)]
pub const SURFACE: Color = Color::Rgb(36, 36, 36);
/// Subtle full-width bar behind fenced code in AI messages (just above BG).
pub const CODE_BG: Color = Color::Rgb(28, 28, 30);
/// Default foreground for unhighlighted / plain code (soft cream — never white).
pub const CODE_TEXT: Color = Color::Rgb(168, 166, 158);
/// User message cell background (dim, subtle lift off terminal bg).
pub const USER_BG: Color = Color::Rgb(34, 34, 34);
/// Input composer background (slightly lighter than user cell, still dim).
pub const INPUT_BG: Color = Color::Rgb(30, 30, 30);
/// Footer / bottom bar text (dim grey, not too dark).
pub const FOOTER: Color = Color::Rgb(130, 130, 130);
/// Lead icon on user messages (muted grey).
pub const USER_LEAD: Color = Color::Rgb(150, 150, 150);
/// Lead icon on the input bar.
pub const INPUT_LEAD: Color = Color::Rgb(240, 240, 240);
/// Selected row / highlight fill (dark orange-gray).
pub const SELECT_BG: Color = Color::Rgb(70, 48, 28);
/// Borders and rules.
pub const BORDER: Color = Color::Rgb(72, 72, 72);

pub const SUCCESS: Color = Color::Rgb(120, 185, 105);
pub const ERROR: Color = Color::Rgb(220, 95, 75);
pub const WARNING: Color = Color::Rgb(220, 170, 70);

/// Diff line colors (keep readable; not purple).
pub const DIFF_ADD: Color = Color::Rgb(100, 185, 120);
pub const DIFF_DEL: Color = Color::Rgb(220, 115, 105);
pub const DIFF_META: Color = Color::Rgb(120, 120, 120);
/// Diff line background colors (subtle tints for changed lines).
pub const DIFF_ADD_BG: Color = Color::Rgb(30, 50, 35);
pub const DIFF_DEL_BG: Color = Color::Rgb(55, 30, 28);

// ---------------------------------------------------------------------------
// Markdown element colors (assistant transcript)
// ---------------------------------------------------------------------------

/// Paragraph / normal prose — white is fine for main body.
pub const MD_TEXT: Color = Color::Rgb(225, 225, 225);
/// Headings h1 + links.
pub const MD_HEADING1: Color = ACCENT;
/// Headings h2 — warmer cream, readable but not white.
pub const MD_HEADING2: Color = Color::Rgb(214, 198, 178);
/// Headings h3 / secondary emphasis.
pub const MD_HEADING3: Color = Color::Rgb(168, 148, 128);
/// Blockquotes, list markers, horizontal rules, table borders.
pub const MD_MUTED: Color = Color::Rgb(118, 114, 108);
/// Inline `code` spans.
pub const MD_INLINE_CODE: Color = Color::Rgb(214, 176, 110);
/// Links (and other primary accents in markdown).
pub const MD_LINK: Color = Color::Rgb(224, 140, 100);
/// Info-ish secondary (tables / callouts via info_color).
pub const MD_INFO: Color = Color::Rgb(130, 165, 155);

// ---------------------------------------------------------------------------
// Syntax highlight (fenced code + diffs) — GrokNight-soft suite
// ---------------------------------------------------------------------------

pub const SYN_COMMENT: Color = Color::Rgb(110, 108, 102);
pub const SYN_KEYWORD: Color = Color::Rgb(214, 130, 110);
pub const SYN_STRING: Color = Color::Rgb(152, 180, 130);
pub const SYN_STRING_ESCAPE: Color = Color::Rgb(178, 198, 150);
pub const SYN_NUMBER: Color = Color::Rgb(210, 170, 110);
pub const SYN_CONSTANT: Color = Color::Rgb(200, 155, 130);
pub const SYN_FUNCTION: Color = Color::Rgb(130, 175, 190);
pub const SYN_TYPE: Color = Color::Rgb(145, 175, 160);
/// Identifiers / plain tokens — dimmer than body text, not white.
pub const SYN_VARIABLE: Color = Color::Rgb(168, 166, 158);
pub const SYN_PROPERTY: Color = Color::Rgb(160, 170, 185);
pub const SYN_OPERATOR: Color = Color::Rgb(150, 145, 140);
pub const SYN_PUNCTUATION: Color = Color::Rgb(120, 118, 114);
pub const SYN_ATTRIBUTE: Color = Color::Rgb(200, 170, 120);
pub const SYN_TAG: Color = Color::Rgb(130, 175, 190);
pub const SYN_LABEL: Color = Color::Rgb(200, 140, 130);
pub const SYN_ERROR: Color = Color::Rgb(220, 100, 90);

/// Syntax colors used by tree-sitter highlighting.
pub fn code_syntax_colors() -> CodeColors {
    CodeColors {
        comment: SYN_COMMENT,
        keyword: SYN_KEYWORD,
        string: SYN_STRING,
        string_escape: SYN_STRING_ESCAPE,
        number: SYN_NUMBER,
        constant: SYN_CONSTANT,
        function: SYN_FUNCTION,
        r#type: SYN_TYPE,
        variable: SYN_VARIABLE,
        property: SYN_PROPERTY,
        operator: SYN_OPERATOR,
        punctuation: SYN_PUNCTUATION,
        attribute: SYN_ATTRIBUTE,
        tag: SYN_TAG,
        label: SYN_LABEL,
        error: SYN_ERROR,
    }
}

/// Full `ratatui-markdown` theme for assistant markdown rendering.
///
/// Element mapping:
/// - body / bold / italic → `MD_TEXT` (white/near-white OK)
/// - h1 (hooks) → `MD_HEADING1` accent; links → `MD_LINK`
/// - h2 / h3 (hooks) → cream / taupe hierarchy
/// - inline `code` → `MD_INLINE_CODE` amber
/// - lists / quotes / rules → `MD_MUTED`
/// - fenced code → tree-sitter + `code_syntax_colors()` (plain tokens use `CODE_TEXT`)
pub fn markdown_theme() -> ThemeConfig {
    ThemeConfig {
        text_color: MD_TEXT,
        muted_text_color: MD_MUTED,
        // Links and other primary accents (headings use RenderHooks).
        primary_color: MD_LINK,
        popup_selected_background: SELECT_BG,
        border_color: BORDER,
        focused_border_color: ACCENT,
        secondary_color: MD_HEADING3,
        info_color: MD_INFO,
        json_key_color: SYN_PROPERTY,
        json_string_color: SYN_STRING,
        json_number_color: SYN_NUMBER,
        json_bool_color: SYN_KEYWORD,
        json_null_color: SYN_COMMENT,
        accent_yellow: MD_INLINE_CODE,
        code_colors: code_syntax_colors(),
        ..ThemeConfig::default()
    }
}
