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
//! Module layout:
//! - [`shimmer`] — animated highlight helpers
//! - [`cells`]   — the `HistoryCell` trait and concrete cell types
//! - [`slash`]   — slash-command table + fuzzy filter
//! - [`render`]  — shared per-frame [`render::RenderContext`]
//! - [`app`]     — the `App` state machine, event handlers, draw, and `run`

mod shimmer;
mod cells;
mod slash;
mod render;
mod app;

pub use app::run;
