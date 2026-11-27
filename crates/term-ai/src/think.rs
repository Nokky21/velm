use crate::errors::AiError;

const OPEN_TAG: &str = "<think>";
const CLOSE_TAG: &str = "</think>";

/// Outcome returned by [`ThinkFilter::push`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ThinkDelta {
    /// Text outside `<think>...</think>` ready to emit now.
    pub text: String,
    /// `true` if an orphan `</think>` invalidated previously emitted text.
    pub reset_prior: bool,
}

/// Streaming filter for `<think>...</think>` reasoning blocks (CONTRATO §C.5).
#[derive(Debug, Clone, Default)]
pub struct ThinkFilter {
    in_think: bool,
    pending: String,
    accumulated: String,
}

impl ThinkFilter {
    /// Creates a new streaming reasoning filter.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds one delta chunk from the stream and returns newly safe output.
    ///
    /// Partial `<think>` or `</think>` tags split across chunks are buffered until
    /// resolved. An orphan `</think>` outside a `<think>` block discards all prior
    /// accumulated text per CONTRATO §C.5.
    pub fn push(&mut self, chunk: &str) -> ThinkDelta {
        if chunk.is_empty() {
            return ThinkDelta::default();
        }

        let mut buf = if self.pending.is_empty() {
            chunk.to_owned()
        } else {
            let mut combined = std::mem::take(&mut self.pending);
            combined.push_str(chunk);
            combined
        };

        let mut delta = ThinkDelta::default();

        loop {
            if self.in_think {
                if let Some(pos) = buf.find(CLOSE_TAG) {
                    buf.drain(..pos + CLOSE_TAG.len());
                    self.in_think = false;
                    continue;
                }
                let keep = trailing_tag_prefix_len(&buf, CLOSE_TAG);
                if keep > 0 {
                    let tail_start = buf.len() - keep;
                    self.pending.push_str(&buf[tail_start..]);
                }
                break;
            }

            let open_pos = buf.find(OPEN_TAG);
            let close_pos = buf.find(CLOSE_TAG);

            match (open_pos, close_pos) {
                (Some(op), Some(cp)) if cp < op => {
                    // Orphan closing tag appeared before any opening tag: discard prior text.
                    self.accumulated.clear();
                    delta.text.clear();
                    delta.reset_prior = true;
                    buf.drain(..cp + CLOSE_TAG.len());
                }
                (None, Some(cp)) => {
                    // Orphan closing tag without opening tag: discard prior text.
                    self.accumulated.clear();
                    delta.text.clear();
                    delta.reset_prior = true;
                    buf.drain(..cp + CLOSE_TAG.len());
                }
                (Some(op), _) => {
                    if op > 0 {
                        let visible = &buf[..op];
                        self.accumulated.push_str(visible);
                        delta.text.push_str(visible);
                    }
                    buf.drain(..op + OPEN_TAG.len());
                    self.in_think = true;
                }
                (None, None) => {
                    let keep = trailing_tag_prefix_len(&buf, OPEN_TAG)
                        .max(trailing_tag_prefix_len(&buf, CLOSE_TAG));
                    let emit_end = buf.len() - keep;
                    if emit_end > 0 {
                        let visible = &buf[..emit_end];
                        self.accumulated.push_str(visible);
                        delta.text.push_str(visible);
                    }
                    if keep > 0 {
                        self.pending.push_str(&buf[emit_end..]);
                    }
                    break;
                }
            }
        }

        delta
    }

    /// Finalizes the stream and returns the cleaned response text.
    ///
    /// Returns [`AiError::Thinking`] when `<think>` was never closed and no
    /// non-whitespace output was produced outside `<think>`.
    pub fn finish(mut self) -> Result<String, AiError> {
        if !self.in_think && !self.pending.is_empty() {
            self.accumulated.push_str(&self.pending);
        }

        if self.in_think && self.accumulated.trim().is_empty() {
            return Err(AiError::Thinking);
        }

        Ok(self.accumulated)
    }

    /// Flushes any trailing non-tag buffered text at stream end (for streaming consumers).
    pub fn finish_delta(self) -> Result<String, AiError> {
        let mut tail = String::new();
        if !self.in_think && !self.pending.is_empty() {
            tail.push_str(&self.pending);
        }
        if self.in_think && self.accumulated.trim().is_empty() {
            return Err(AiError::Thinking);
        }
        Ok(tail)
    }
}

fn trailing_tag_prefix_len(text: &str, tag: &str) -> usize {
    let max_check = text.len().min(tag.len().saturating_sub(1));
    for len in (1..=max_check).rev() {
        let start = text.len() - len;
        if text.is_char_boundary(start) && tag.starts_with(&text[start..]) {
            return len;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_split_think_tags_across_chunks() {
        let mut filter = ThinkFilter::new();
        let d1 = filter.push("<thi");
        assert_eq!(d1.text, "");
        let d2 = filter.push("nk>reasoning here</th");
        assert_eq!(d2.text, "");
        let d3 = filter.push("ink>\nls -la");
        assert_eq!(d3.text, "\nls -la");
        let out = filter.finish().expect("closed think");
        assert_eq!(out, "\nls -la");
    }

    #[test]
    fn unclosed_think_returns_thinking_error() {
        let mut filter = ThinkFilter::new();
        filter.push("  <think>still thinking");
        filter.push(" forever");
        assert_eq!(filter.finish(), Err(AiError::Thinking));
    }

    #[test]
    fn orphan_closing_tag_discards_all_prior_text() {
        let mut filter = ThinkFilter::new();
        let d1 = filter.push("hidden reasoning without opening tag");
        assert_eq!(d1.text, "hidden reasoning without opening tag");
        let d2 = filter.push("</think>\n```zsh\npwd\n```");
        assert!(d2.reset_prior);
        assert_eq!(d2.text, "\n```zsh\npwd\n```");
        assert_eq!(filter.finish().unwrap(), "\n```zsh\npwd\n```");
    }

    #[test]
    fn orphan_closing_tag_split_across_chunks() {
        let mut filter = ThinkFilter::new();
        let _ = filter.push("secret thought</th");
        let d2 = filter.push("ink>answer");
        assert!(d2.reset_prior);
        assert_eq!(d2.text, "answer");
        assert_eq!(filter.finish().unwrap(), "answer");
    }

    #[test]
    fn keeps_incomplete_angle_bracket_at_eof_outside_think() {
        let mut filter = ThinkFilter::new();
        let d1 = filter.push("echo a <t");
        assert_eq!(d1.text, "echo a ");
        assert_eq!(filter.finish().unwrap(), "echo a <t");
    }

    #[test]
    fn handles_multiple_think_blocks() {
        let mut filter = ThinkFilter::new();
        filter.push("<think>a</think>one<think>b</think>two");
        assert_eq!(filter.finish().unwrap(), "onetwo");
    }
}
