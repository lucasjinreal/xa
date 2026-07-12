//! Gray + orange TUI palette.
//!
//! Deliberately no purple / magenta / blue-violet accents. Surfaces are neutral
//! grays; interactive / brand emphasis is warm orange.

use ratatui::style::Color;

/// Brand accent (warm orange).
pub const ACCENT: Color = Color::Rgb(232, 140, 60);
/// Softer orange for secondary emphasis / selected idle states.
pub const ACCENT_DIM: Color = Color::Rgb(190, 120, 60);
/// Bright peak used by shimmer highlights.
pub const ACCENT_BRIGHT: Color = Color::Rgb(255, 190, 120);

/// Primary body text.
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
