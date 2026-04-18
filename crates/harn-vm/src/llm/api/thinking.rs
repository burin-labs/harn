//! `<think>...</think>` block splitters used by OpenAI-compatible
//! providers (Qwen3/Qwen3.5 via vLLM's
//! `chat_template_kwargs.enable_thinking`). Both the batch and streaming
//! splitters live here so the detection logic never drifts between them.

/// Split `<think>...</think>` blocks out of an OpenAI-compatible response
/// text. Returns `(visible_text, thinking_text)`. Handles multiple thinking
/// blocks, malformed/unclosed tags (best-effort), and preserves original
/// whitespace in the visible portion.
pub(crate) fn split_openai_thinking_blocks(raw: &str) -> (String, String) {
    if !raw.contains("<think>") {
        return (raw.to_string(), String::new());
    }
    let mut visible = String::new();
    let mut thinking = String::new();
    let mut rest = raw;
    loop {
        if let Some(start) = rest.find("<think>") {
            visible.push_str(&rest[..start]);
            let after_tag = &rest[start + "<think>".len()..];
            if let Some(end) = after_tag.find("</think>") {
                thinking.push_str(&after_tag[..end]);
                rest = &after_tag[end + "</think>".len()..];
            } else {
                // Unclosed <think>: treat everything after as thinking.
                thinking.push_str(after_tag);
                break;
            }
        } else {
            visible.push_str(rest);
            break;
        }
    }
    // Models emit `<think>...</think>\nActual`; strip the blank line.
    let visible = visible.trim_start_matches('\n').to_string();
    (visible, thinking.trim().to_string())
}

/// Incremental splitter for OpenAI-style streaming content that may contain
/// `<think>...</think>` blocks. Buffers a small suffix to handle tags split
/// across delta chunks. Only emits visible (non-thinking) content to the
/// delta channel; accumulates thinking separately for the final result.
#[derive(Default)]
pub(crate) struct ThinkingStreamSplitter {
    /// True while we're inside a `<think>` block.
    in_thinking: bool,
    /// Carryover characters from the last delta that might be the start of a
    /// `<think>` or `</think>` tag. Never longer than `</think>`.len() - 1.
    carry: String,
    /// Accumulated thinking text (returned at the end).
    pub thinking: String,
}

impl ThinkingStreamSplitter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a new delta chunk. Returns the visible portion to forward
    /// downstream (may be empty if the entire chunk was part of a thinking
    /// block or got held back as carry).
    pub fn push(&mut self, delta: &str) -> String {
        let combined = {
            let mut s = std::mem::take(&mut self.carry);
            s.push_str(delta);
            s
        };
        let mut visible_out = String::new();
        let mut cursor = 0usize;
        let bytes = combined.as_bytes();
        while cursor < bytes.len() {
            if self.in_thinking {
                // Look for </think> in the remainder.
                if let Some(rel) = combined[cursor..].find("</think>") {
                    self.thinking.push_str(&combined[cursor..cursor + rel]);
                    cursor += rel + "</think>".len();
                    self.in_thinking = false;
                } else {
                    // Hold back up to len("</think>")-1 chars as potential
                    // split-tag carry; emit the rest into thinking.
                    let hold = "</think>".len() - 1;
                    let remaining = combined.len() - cursor;
                    if remaining <= hold {
                        self.carry.push_str(&combined[cursor..]);
                    } else {
                        let mut split = combined.len() - hold;
                        while split > cursor && !combined.is_char_boundary(split) {
                            split -= 1;
                        }
                        self.thinking.push_str(&combined[cursor..split]);
                        self.carry.push_str(&combined[split..]);
                    }
                    return visible_out;
                }
            } else {
                // Look for <think> in the remainder.
                if let Some(rel) = combined[cursor..].find("<think>") {
                    visible_out.push_str(&combined[cursor..cursor + rel]);
                    cursor += rel + "<think>".len();
                    self.in_thinking = true;
                } else {
                    // Hold back len("<think>")-1 chars as potential split-tag
                    // carry; emit the rest as visible.
                    let hold = "<think>".len() - 1;
                    let remaining = combined.len() - cursor;
                    if remaining <= hold {
                        self.carry.push_str(&combined[cursor..]);
                    } else {
                        let mut split = combined.len() - hold;
                        // Floor to char boundary to avoid slicing inside a
                        // multi-byte UTF-8 codepoint.
                        while split > cursor && !combined.is_char_boundary(split) {
                            split -= 1;
                        }
                        visible_out.push_str(&combined[cursor..split]);
                        self.carry.push_str(&combined[split..]);
                    }
                    return visible_out;
                }
            }
        }
        visible_out
    }

    /// Flush any remaining carry as visible or thinking, depending on state.
    /// Called when the stream terminates.
    pub fn flush(&mut self) -> String {
        let rest = std::mem::take(&mut self.carry);
        if self.in_thinking {
            self.thinking.push_str(&rest);
            String::new()
        } else {
            rest
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{split_openai_thinking_blocks, ThinkingStreamSplitter};

    #[test]
    fn thinking_split_no_tags_returns_original() {
        let (visible, thinking) = split_openai_thinking_blocks("just a plain response");
        assert_eq!(visible, "just a plain response");
        assert_eq!(thinking, "");
    }

    #[test]
    fn thinking_split_single_block() {
        let raw = "<think>step by step reasoning</think>\nThe answer is 42.";
        let (visible, thinking) = split_openai_thinking_blocks(raw);
        assert_eq!(visible, "The answer is 42.");
        assert_eq!(thinking, "step by step reasoning");
    }

    #[test]
    fn thinking_split_multiple_blocks() {
        let raw = "<think>first</think>hello <think>second</think>world";
        let (visible, thinking) = split_openai_thinking_blocks(raw);
        assert_eq!(visible, "hello world");
        assert_eq!(
            thinking,
            "first\nsecond".replace('\n', "") /* joined with empty */
        );
        // Invariant: neither block text leaked into visible.
        assert!(!visible.contains("first"));
        assert!(!visible.contains("second"));
    }

    #[test]
    fn thinking_split_unclosed_block_captures_remainder() {
        let raw = "<think>reasoning with no closing tag and then text";
        let (visible, thinking) = split_openai_thinking_blocks(raw);
        assert_eq!(visible, "");
        assert!(thinking.contains("reasoning with no closing tag"));
    }

    #[test]
    fn thinking_stream_splitter_handles_clean_boundaries() {
        let mut s = ThinkingStreamSplitter::new();
        let v1 = s.push("<think>");
        let v2 = s.push("reasoning");
        let v3 = s.push("</think>");
        let v4 = s.push("visible answer");
        let tail = s.flush();
        assert_eq!(v1, "");
        assert_eq!(v2, "");
        assert_eq!(v3, "");
        let combined = format!("{}{}{}{}{}", v1, v2, v3, v4, tail);
        assert_eq!(combined, "visible answer");
        assert_eq!(s.thinking, "reasoning");
    }

    #[test]
    fn thinking_stream_splitter_handles_split_tags() {
        let mut s = ThinkingStreamSplitter::new();
        let v1 = s.push("<thi");
        let v2 = s.push("nk>inside</thi");
        let v3 = s.push("nk>after");
        let tail = s.flush();
        let combined = format!("{}{}{}{}", v1, v2, v3, tail);
        assert_eq!(combined, "after");
        assert_eq!(s.thinking, "inside");
    }

    #[test]
    fn thinking_stream_splitter_passthrough_without_tags() {
        let mut s = ThinkingStreamSplitter::new();
        let v1 = s.push("hello ");
        let v2 = s.push("world");
        let tail = s.flush();
        let combined = format!("{}{}{}", v1, v2, tail);
        assert_eq!(combined, "hello world");
        assert_eq!(s.thinking, "");
    }
}
