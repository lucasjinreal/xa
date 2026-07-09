//! Shared render context passed to every [`HistoryCell`] during a frame.

/// Context shared across all cell renders within a single frame.
pub struct RenderContext {
    pub shimmer_phase: f32,
}
