//! Source-geometry projection of a prompt template: where every block
//! construct opens and where it closes.
//!
//! The render AST deliberately drops geometry — it only needs enough
//! position to attribute output and errors back to a directive. Editors
//! need the opposite: the byte range a construct covers, so a folding
//! range or a block-aware selection lands on real boundaries.
//!
//! This is a read-only view produced by the one template parser (the
//! same call `render` makes), so it can never disagree with what the
//! engine actually parsed. It is the sibling of [`super::lint`], which
//! projects the same parse for lint rules.

use super::error::TemplateParseError;
use super::parser::parse_outline;

/// One foldable block in a template, as a byte range over the source
/// that was parsed.
///
/// `start` is the first byte of the directive that opens the block;
/// `end` is one past the last byte of the directive that closes it. A
/// branch inside an `{{ if }}` chain opens at its own `{{ elif }}` /
/// `{{ else }}` and closes at the chain's shared `{{ end }}`, so nested
/// branches produce nested ranges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutlineBlock {
    pub kind: OutlineBlockKind,
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutlineBlockKind {
    /// `{{ if .. }}` through the chain's `{{ end }}`.
    If,
    /// `{{ elif .. }}` through the chain's `{{ end }}`.
    Elif,
    /// `{{ else }}` through the enclosing `{{ end }}`. Covers both the
    /// `{{ if }}` and the `{{ for }}` (empty-iterable) forms.
    Else,
    /// `{{ for .. }}` through its `{{ end }}`.
    For,
    /// `{{ section ".." }}` through its `{{ endsection }}`.
    Section,
    /// `{{ raw }}` through its `{{ endraw }}`.
    Raw,
    /// `{{# .. #}}`.
    Comment,
}

/// Parse `src` and return its block outline in source order.
///
/// Returns `Err` when the template doesn't parse — the same failure
/// `render` would report, so a caller that already surfaced a parse
/// diagnostic can simply drop the outline.
pub fn parse(src: &str) -> Result<Vec<OutlineBlock>, TemplateParseError> {
    parse_outline(src).map_err(TemplateParseError::from)
}

#[cfg(test)]
mod tests {
    use super::{parse, OutlineBlockKind};

    /// Every block as `(kind, the source text it covers)`, so an
    /// expectation reads as the template it describes.
    fn blocks(src: &str) -> Vec<(OutlineBlockKind, &str)> {
        parse(src)
            .expect("template parses")
            .into_iter()
            .map(|block| (block.kind, &src[block.start..block.end]))
            .collect()
    }

    #[test]
    fn every_branch_of_a_chain_closes_at_the_shared_end() {
        let src = "{{ if a }}A{{ elif b }}B{{ else }}C{{ end }}";
        assert_eq!(
            blocks(src),
            vec![
                (OutlineBlockKind::If, src),
                (OutlineBlockKind::Elif, "{{ elif b }}B{{ else }}C{{ end }}"),
                (OutlineBlockKind::Else, "{{ else }}C{{ end }}"),
            ]
        );
    }

    #[test]
    fn a_loop_covers_its_empty_fallback() {
        let src = "{{ for x in xs }}{{ x }}{{ else }}none{{ end }}";
        assert_eq!(
            blocks(src),
            vec![
                (OutlineBlockKind::For, src),
                (OutlineBlockKind::Else, "{{ else }}none{{ end }}"),
            ]
        );
    }

    #[test]
    fn sections_raw_blocks_and_comments_are_all_located() {
        let src = "{{# note #}}{{ section \"task\" }}{{ raw }}{{x}}{{ endraw }}{{ endsection }}";
        assert_eq!(
            blocks(src),
            vec![
                (OutlineBlockKind::Comment, "{{# note #}}"),
                (
                    OutlineBlockKind::Section,
                    "{{ section \"task\" }}{{ raw }}{{x}}{{ endraw }}{{ endsection }}"
                ),
                (OutlineBlockKind::Raw, "{{ raw }}{{x}}{{ endraw }}"),
            ]
        );
    }

    #[test]
    fn nested_blocks_come_back_in_source_order() {
        let src = "{{ for x in xs }}{{ if x }}y{{ end }}{{ end }}";
        assert_eq!(
            blocks(src),
            vec![
                (OutlineBlockKind::For, src),
                (OutlineBlockKind::If, "{{ if x }}y{{ end }}"),
            ]
        );
    }

    #[test]
    fn a_template_without_blocks_has_an_empty_outline() {
        assert_eq!(blocks("Hello {{ name }}, welcome."), Vec::new());
    }

    #[test]
    fn an_unclosed_block_reports_where_it_opened() {
        let error = parse("intro\n{{ if a }}\nbody\n").expect_err("unclosed `{{ if }}`");
        assert_eq!((error.line, error.col), (2, 1));
        assert!(
            error.message.contains("missing matching `{{ end }}`"),
            "unexpected message: {}",
            error.message
        );
    }

    #[test]
    fn trim_markers_do_not_shift_a_block_off_its_directive() {
        let src = "{{- if a -}}A{{- end -}}";
        assert_eq!(blocks(src), vec![(OutlineBlockKind::If, src)]);
    }
}
