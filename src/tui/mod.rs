//! Codex-like interactive TUI for `xa`.
//!
//! Built on ratatui + crossterm with an Elm-ish event loop. Agent output is
//! rendered as a sequence of independent [`cells::HistoryCell`]s (user messages,
//! assistant markdown, tool-call cards, errors, system notes) with simple
//! virtual scrolling. The transcript follows the DESIGN.md "HistoryCell"
//! pattern rather than one giant scrollable paragraph.
//!
//! Slash commands (`/login`, `/models`, `/clear`, `/help`, `/exit`, ...) are
//! handled via a floating popup overlay driven by a fuzzy subsequence filter.
//!
//! The provider / model setup flow (`/login`, `/models`) is a codex-style
//! interactive wizard rendered above the composer — see [`wizard`].
//!
//! Module layout:
//! - [`theme`]   — dark/light gray + orange palette (auto-detectable)
//! - [`shimmer`] — animated highlight helpers
//! - [`cells`]   — the `HistoryCell` trait and concrete cell types
//! - [`think`]   — stream phase + `<think>` filter
//! - [`slash`]   — slash-command table + fuzzy filter
//! - [`render`]  — shared per-frame [`render::RenderContext`]
//! - [`wizard`]  — in-TUI provider / model selection panel
//! - [`app`]     — the `App` state machine, event handlers, draw, and `run`

mod theme;
mod shimmer;
mod cells;
mod think;
mod slash;
mod render;
pub mod wizard;
pub mod resume;
mod app;
mod crash;

pub use app::run;
pub use theme::{init_from_preference, ColorMode, ThemePreference};
