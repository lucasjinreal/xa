//! Stream-phase tracking and `<think>`…`</think>` filtering.
//!
//! While the model streams, content between think tags is withheld from the
//! transcript. The activity strip above the input bar reports:
//! - **Waiting for response…** — turn started, no tokens yet
//! - **Thinking…** — inside a `<think>` block
//! - **Responding…** — visible answer tokens are flowing

/// Claude-Code-style activity label shown above the input bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamPhase {
    Idle,
    /// Submitted; waiting for the first model tokens.
    Waiting,
    /// Inside a `<think>`…`</think>` block (tags stripped from the transcript).
    Thinking,
    /// Visible answer content is streaming.
    Responding,
    /// A tool call is in flight.
    RunningTool,
    /// Retrying after a transient error.
    Retrying { attempt: u32, max: u32 },
    /// User cancelled the in-flight turn (ESC).
    Interrupted,
    Error,
}

impl StreamPhase {
    pub fn label(self) -> Option<String> {
        match self {
            StreamPhase::Idle => None,
            StreamPhase::Waiting => Some("Waiting for response\u{2026}".into()),
            StreamPhase::Thinking => Some("Thinking\u{2026}".into()),
            StreamPhase::Responding => Some("Responding\u{2026}".into()),
            StreamPhase::RunningTool => Some("Running tools\u{2026}".into()),
            StreamPhase::Retrying { attempt, max } => {
                Some(format!("Retrying ({attempt}/{max})\u{2026}"))
            }
            StreamPhase::Interrupted => Some("interrupted".into()),
            StreamPhase::Error => Some("Error".into()),
        }
    }

    pub fn is_active(self) -> bool {
        !matches!(self, StreamPhase::Idle)
    }

    /// Terminal phases keep a sticky activity line until the next keystroke.
    pub fn is_terminal(self) -> bool {
        matches!(self, StreamPhase::Interrupted | StreamPhase::Error)
    }
}

const OPEN: &str = "<think>";
const CLOSE: &str = "</think>";

/// Incremental filter for streamed assistant text.
///
/// Handles tags that span chunk boundaries. Only text *outside* think blocks is
/// returned as visible content for the transcript.
#[derive(Debug, Default)]
pub struct ThinkFilter {
    inside: bool,
    /// Unconsumed tail that might be a partial open/close tag.
    hold: String,
    /// True once any visible (non-think) text has been emitted this turn.
    pub saw_visible: bool,
}

impl ThinkFilter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }

    pub fn inside_think(&self) -> bool {
        self.inside
    }

    /// Feed a stream chunk. Returns visible text (may be empty) and the phase
    /// implied by the filter after this chunk (Waiting is never returned here —
    /// the caller upgrades Waiting → Thinking/Responding based on this).
    pub fn feed(&mut self, chunk: &str) -> (String, bool /*inside_think*/) {
        self.hold.push_str(chunk);
        let mut visible = String::new();

        loop {
            if self.inside {
                if let Some(i) = self.hold.find(CLOSE) {
                    // Drop thinking body + close tag.
                    self.hold = self.hold[i + CLOSE.len()..].to_string();
                    self.inside = false;
                    continue;
                }
                // Keep a suffix that could still become `</think>`.
                let keep = partial_suffix(&self.hold, CLOSE);
                self.hold = self.hold[self.hold.len() - keep..].to_string();
                break;
            }

            if let Some(i) = self.hold.find(OPEN) {
                let before = &self.hold[..i];
                if !before.is_empty() {
                    visible.push_str(before);
                    self.saw_visible = true;
                }
                self.hold = self.hold[i + OPEN.len()..].to_string();
                self.inside = true;
                continue;
            }

            // Not inside think and no full open tag: emit safe prefix, hold
            // possible partial `<think`.
            let keep = partial_suffix(&self.hold, OPEN);
            let emit_end = self.hold.len() - keep;
            if emit_end > 0 {
                visible.push_str(&self.hold[..emit_end]);
                self.saw_visible = true;
                self.hold = self.hold[emit_end..].to_string();
            }
            break;
        }

        (visible, self.inside)
    }

    /// Flush any held text at end of stream (treat partial tags as literal).
    pub fn finish(&mut self) -> String {
        if self.inside {
            // Unclosed think: drop held thinking residue.
            self.hold.clear();
            self.inside = false;
            return String::new();
        }
        let rest = std::mem::take(&mut self.hold);
        if !rest.is_empty() {
            self.saw_visible = true;
        }
        rest
    }
}

/// Length of the longest suffix of `s` that is a prefix of `tag`.
fn partial_suffix(s: &str, tag: &str) -> usize {
    let max = s.len().min(tag.len().saturating_sub(1));
    for len in (1..=max).rev() {
        if s.ends_with(&tag[..len]) {
            return len;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupted_is_terminal_not_error() {
        assert_eq!(StreamPhase::Interrupted.label().as_deref(), Some("interrupted"));
        assert!(StreamPhase::Interrupted.is_terminal());
        assert!(StreamPhase::Error.is_terminal());
        assert!(!StreamPhase::Waiting.is_terminal());
        assert!(StreamPhase::Interrupted.is_active());
    }

    #[test]
    fn strips_think_block() {
        let mut f = ThinkFilter::new();
        let (v, inside) = f.feed("Hi <think>secret</think> there");
        assert!(!inside);
        assert_eq!(v, "Hi  there");
    }

    #[test]
    fn spans_chunks() {
        let mut f = ThinkFilter::new();
        let (v1, i1) = f.feed("a <th");
        assert_eq!(v1, "a ");
        assert!(!i1);
        let (v2, i2) = f.feed("ink>hidden");
        assert!(v2.is_empty());
        assert!(i2);
        let (v3, i3) = f.feed("</thi");
        assert!(v3.is_empty());
        assert!(i3);
        let (v4, i4) = f.feed("nk>out");
        assert!(!i4);
        assert_eq!(v4, "out");
    }
}
