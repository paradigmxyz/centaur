//! Streaming parser that hides inline tags and extracts their contents.
//!
//! Adapted from `InlineHiddenTagParser` in openai/codex's
//! `codex-utils-stream-parser` (Apache-2.0), revision
//! e93dc98a48d597df322436ffe8d03bfd7ec63b3b, without the generic tag type.

/// Output for one pushed chunk or the final flush.
#[derive(Debug, Default)]
pub(super) struct InlineTagChunk {
    /// Text safe to render immediately.
    pub(super) visible_text: String,
    /// Contents of tags closed in this chunk.
    pub(super) extracted: Vec<String>,
}

/// Literal open and close delimiters of one hidden tag.
#[derive(Debug, Clone, Copy)]
pub(super) struct InlineTagSpec {
    pub(super) open: &'static str,
    pub(super) close: &'static str,
}

/// Hides the configured tags and extracts their contents.
///
/// Matching is literal and non-nested. At the same offset the longest opener
/// wins. A tag still open at the end of the stream is closed and extracted.
#[derive(Debug)]
pub(super) struct InlineTagParser {
    specs: Vec<InlineTagSpec>,
    pending: String,
    active: Option<ActiveTag>,
}

#[derive(Debug)]
struct ActiveTag {
    close: &'static str,
    content: String,
}

impl InlineTagParser {
    pub(super) fn new(specs: Vec<InlineTagSpec>) -> Self {
        Self {
            specs,
            pending: String::new(),
            active: None,
        }
    }

    pub(super) fn push_str(&mut self, chunk: &str) -> InlineTagChunk {
        self.pending.push_str(chunk);
        let mut out = InlineTagChunk::default();

        loop {
            if let Some(active) = &mut self.active {
                if let Some(close_idx) = self.pending.find(active.close) {
                    active.content.push_str(&self.pending[..close_idx]);
                    self.pending.drain(..close_idx + active.close.len());
                    let active = self.active.take().expect("active tag");
                    out.extracted.push(active.content);
                    continue;
                }

                let keep = longest_suffix_prefix_len(&self.pending, active.close);
                let take = self.pending.len() - keep;
                active.content.push_str(&self.pending[..take]);
                self.pending.drain(..take);
                break;
            }

            if let Some((open_idx, spec)) = self.find_next_open() {
                out.visible_text.push_str(&self.pending[..open_idx]);
                self.pending.drain(..open_idx + spec.open.len());
                self.active = Some(ActiveTag {
                    close: spec.close,
                    content: String::new(),
                });
                continue;
            }

            let keep = self
                .specs
                .iter()
                .map(|spec| longest_suffix_prefix_len(&self.pending, spec.open))
                .max()
                .unwrap_or(0);
            let take = self.pending.len() - keep;
            out.visible_text.push_str(&self.pending[..take]);
            self.pending.drain(..take);
            break;
        }

        out
    }

    pub(super) fn finish(&mut self) -> InlineTagChunk {
        let mut out = InlineTagChunk::default();
        let pending = std::mem::take(&mut self.pending);
        match self.active.take() {
            Some(mut active) => {
                active.content.push_str(&pending);
                out.extracted.push(active.content);
            }
            None => out.visible_text = pending,
        }
        out
    }

    /// The earliest opener in `pending`, preferring the longest at one offset.
    fn find_next_open(&self) -> Option<(usize, InlineTagSpec)> {
        self.specs
            .iter()
            .filter_map(|spec| self.pending.find(spec.open).map(|pos| (pos, *spec)))
            .min_by(|(pos_a, spec_a), (pos_b, spec_b)| {
                pos_a
                    .cmp(pos_b)
                    .then_with(|| spec_b.open.len().cmp(&spec_a.open.len()))
            })
    }
}

/// Length of the longest proper prefix of `needle` that `s` ends with.
fn longest_suffix_prefix_len(s: &str, needle: &str) -> usize {
    let max = s.len().min(needle.len().saturating_sub(1));
    (1..=max)
        .rev()
        .find(|&k| needle.is_char_boundary(k) && s.ends_with(&needle[..k]))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{InlineTagChunk, InlineTagParser, InlineTagSpec};

    fn parse(specs: Vec<InlineTagSpec>, chunks: &[&str]) -> InlineTagChunk {
        let mut parser = InlineTagParser::new(specs);
        let mut all = InlineTagChunk::default();
        for chunk in chunks.iter().map(|chunk| parser.push_str(chunk)) {
            all.visible_text.push_str(&chunk.visible_text);
            all.extracted.extend(chunk.extracted);
        }
        let tail = parser.finish();
        all.visible_text.push_str(&tail.visible_text);
        all.extracted.extend(tail.extracted);
        all
    }

    #[test]
    fn hides_tags_split_across_chunks_inside_multibyte_delimiters() {
        let out = parse(
            vec![InlineTagSpec {
                open: "<é>",
                close: "</é>",
            }],
            &["a<", "é>中</", "é>b"],
        );

        assert_eq!(out.visible_text, "ab");
        assert_eq!(out.extracted, ["中"]);
    }

    #[test]
    fn prefers_longest_opener_at_same_offset() {
        let out = parse(
            vec![
                InlineTagSpec {
                    open: "<a>",
                    close: "</a>",
                },
                InlineTagSpec {
                    open: "<ab>",
                    close: "</ab>",
                },
            ],
            &["x<ab>y</ab>z<a>w</a>"],
        );

        assert_eq!(out.visible_text, "xz");
        assert_eq!(out.extracted, ["y", "w"]);
    }

    #[test]
    fn extracts_unclosed_tag_at_end_of_stream() {
        let out = parse(
            vec![InlineTagSpec {
                open: "<a>",
                close: "</a>",
            }],
            &["x<a>tail"],
        );

        assert_eq!(out.visible_text, "x");
        assert_eq!(out.extracted, ["tail"]);
    }
}
