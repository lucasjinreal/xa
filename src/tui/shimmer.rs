//! Shimmer (DESIGN.md §8).
//!
//! Helpers for rendering a moving highlight band over text, used to indicate
//! "active / streaming" regions of the transcript.

use ratatui::style::{Color, Style};
use ratatui::text::Span;

/// Blend two 4-bit/truecolor `Color`s by `t` in [0,1] (1 => `to`).
fn blend(base: Color, to: Color, t: f32) -> Color {
    let (br, bg, bb) = to_rgb(base);
    let (tr, tg, tb) = to_rgb(to);
    let r = (br as f32 + (tr as f32 - br as f32) * t) as u8;
    let g = (bg as f32 + (tg as f32 - bg as f32) * t) as u8;
    let b = (bb as f32 + (tb as f32 - bb as f32) * t) as u8;
    Color::Rgb(r, g, b)
}

fn to_rgb(c: Color) -> (u8, u8, u8) {
    match c {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::White => (255, 255, 255),
        Color::Gray => (190, 190, 190),
        Color::DarkGray => (110, 110, 110),
        Color::Black => (0, 0, 0),
        Color::Yellow => (255, 215, 0),
        Color::Green => (0, 200, 0),
        Color::Red => (220, 50, 50),
        Color::Cyan => (0, 200, 200),
        Color::Magenta => (200, 0, 200),
        Color::Blue => (50, 50, 220),
        other => {
            // Fall back to ANSI approximation for named colors we don't list.
            let (r, g, b) = ansi_approx(other);
            (r, g, b)
        }
    }
}

fn ansi_approx(c: Color) -> (u8, u8, u8) {
    // crude: only handle a few extras, default gray
    match c {
        Color::Reset => (200, 200, 200),
        _ => (180, 180, 180),
    }
}

/// Render `text` as a moving shimmer highlight. `phase` is in [0,1); the
/// highlight band sweeps left→right once per `period`.
pub fn shimmer_spans(text: &str, base: Color, phase: f32) -> Vec<Span<'static>> {
    let len = text.chars().count().max(1) as f32;
    let mut out = Vec::with_capacity(text.chars().count());
    for (i, ch) in text.chars().enumerate() {
        let pos = i as f32 / len;
        let dist = (pos - phase).abs().min(1.0 - (pos - phase).abs());
        let highlight = (1.0 - dist * 4.0).clamp(0.0, 1.0);
        let color = blend(base, Color::White, highlight);
        out.push(Span::styled(ch.to_string(), Style::default().fg(color)));
    }
    out
}

/// Current shimmer phase given a start instant and period.
pub fn shimmer_phase(start: std::time::Instant, period: f32) -> f32 {
    (start.elapsed().as_secs_f32() / period).rem_euclid(1.0)
}
