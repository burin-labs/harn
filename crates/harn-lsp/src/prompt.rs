//! Completion and hover for prompt templates.
//!
//! Everything offered here comes from the template engine's own
//! vocabulary — `template::directives`, `template::filters`,
//! `template::SECTIONS` — so the editor cannot suggest a
//! construct the engine would reject. That matters most for block
//! closers: `{{ endif }}` is a valid bare identifier, so the engine
//! accepts it silently and renders the literal text. It is not in the
//! vocabulary, so it is never suggested.
//!
//! Deciding *where* the cursor is, though, cannot come from the parser.
//! Completion runs on a directive the author has not finished typing,
//! which by definition has no `}}` and does not tokenize. The scanning
//! in this module is confined to that one question — which `{{ }}` the
//! cursor is inside, and which slot within it — and never to what the
//! template means.

use harn_vm::stdlib::template::filters::{self, FILTERS};
use harn_vm::stdlib::template::outline::{self, OutlineBlockKind};
use harn_vm::stdlib::template::vocabulary::{self, Directive, DirectiveRole, DIRECTIVES, SECTIONS};
use tower_lsp::lsp_types::*;

use crate::source_text::SourceText;

/// The `{{ … }}` the cursor sits inside.
struct OpenDirective {
    /// Byte offset of the opening `{{`.
    open: usize,
    /// Byte offset just past `{{` and its optional `-` trim marker.
    body_start: usize,
    /// A `{{# comment #}}`, where nothing is suggestable.
    comment: bool,
}

/// Which slot of a directive the cursor is in.
enum Slot<'a> {
    /// The first word. `{{ i` could still become `{{ if }}` or the
    /// interpolation `{{ item }}`, so this slot offers both.
    Leading { partial: &'a str },
    /// After a `|`, naming a filter.
    Filter { partial: &'a str },
    /// Inside the string literal of `{{ section "…" }}`.
    SectionName { partial: &'a str },
    /// Somewhere further into a value expression.
    Expression { partial: &'a str },
    /// Inside some other string literal, where no vocabulary applies.
    StringLiteral,
}

impl<'a> Slot<'a> {
    /// The partial token the cursor is in, which a completion replaces.
    fn partial(&self) -> &'a str {
        match *self {
            Slot::Leading { partial }
            | Slot::Filter { partial }
            | Slot::SectionName { partial }
            | Slot::Expression { partial } => partial,
            Slot::StringLiteral => "",
        }
    }
}

pub(crate) fn completions(source: &SourceText, position: Position) -> Vec<CompletionItem> {
    let offset = source.offset(position);
    let Some(directive) = open_directive_at(source, offset) else {
        return Vec::new();
    };
    if directive.comment || inside_raw_block(source, offset) {
        return Vec::new();
    }
    let body = &source[directive.body_start..offset];
    let slot = classify(body);
    let replace = Range {
        start: source.position(offset - slot.partial().len()),
        end: source.position(offset),
    };

    match slot {
        Slot::Leading { partial } => {
            let mut items = directive_items(partial, replace);
            items.extend(binding_items(
                source,
                directive.open,
                offset,
                partial,
                replace,
            ));
            items
        }
        Slot::Filter { partial } => filter_items(partial, replace),
        Slot::SectionName { partial } => section_items(partial, replace),
        Slot::Expression { partial } => {
            binding_items(source, directive.open, offset, partial, replace)
        }
        Slot::StringLiteral => Vec::new(),
    }
}

pub(crate) fn hover(source: &SourceText, position: Position) -> Option<Hover> {
    let offset = source.offset(position);
    let directive = open_or_enclosing_directive(source, offset)?;
    if directive.comment {
        return None;
    }
    let (word, start, end) = word_at(source, offset)?;
    // A filter and a directive can share a spelling only if the
    // vocabulary gains one; prefer whichever the position supports.
    let markdown = if preceded_by_filter_pipe(source, directive.body_start, start) {
        filters::lookup(&word).map(filter_documentation)
    } else {
        vocabulary::directive(&word).map(directive_documentation)
    }?;

    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: markdown,
        }),
        range: Some(Range {
            start: source.position(start),
            end: source.position(end),
        }),
    })
}

/// The directive the cursor is inside, whether or not it is closed.
/// Hover, unlike completion, runs on text the author has finished.
fn open_or_enclosing_directive(source: &SourceText, offset: usize) -> Option<OpenDirective> {
    if let Some(open) = open_directive_at(source, offset) {
        return Some(open);
    }
    let open = source[..offset].rfind("{{")?;
    let close = source[open..].find("}}")? + open;
    if offset > close + 2 {
        return None;
    }
    Some(directive_at(source, open))
}

fn open_directive_at(source: &str, offset: usize) -> Option<OpenDirective> {
    let before = &source[..offset];
    let open = before.rfind("{{")?;
    // A `}}` between that `{{` and the cursor means the directive is
    // already closed and the cursor is in template text.
    if before[open + 2..].contains("}}") {
        return None;
    }
    Some(directive_at(source, open))
}

fn directive_at(source: &str, open: usize) -> OpenDirective {
    let bytes = source.as_bytes();
    let mut body_start = open + 2;
    if bytes.get(body_start) == Some(&b'-') {
        body_start += 1;
    }
    OpenDirective {
        open,
        body_start,
        comment: bytes.get(open + 2) == Some(&b'#'),
    }
}

/// Whether `offset` falls in a `{{ raw }}` block, where directives are
/// emitted verbatim rather than evaluated. Only knowable when the
/// template currently parses; an unparsed one simply gets suggestions.
fn inside_raw_block(source: &str, offset: usize) -> bool {
    let Ok(blocks) = outline::parse(source) else {
        return false;
    };
    blocks.iter().any(|block| {
        block.kind == OutlineBlockKind::Raw && block.start < offset && offset < block.end
    })
}

/// Scan of a partial directive body: where the last filter pipe is, and
/// whether the cursor ended up inside a string literal.
struct BodyScan {
    last_pipe: Option<usize>,
    open_quote: Option<usize>,
}

fn scan_body(body: &str) -> BodyScan {
    let bytes = body.as_bytes();
    let mut scan = BodyScan {
        last_pipe: None,
        open_quote: None,
    };
    let mut quote: Option<u8> = None;
    let mut quote_at = 0usize;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match quote {
            Some(q) => {
                if b == b'\\' {
                    i += 2;
                    continue;
                }
                if b == q {
                    quote = None;
                }
            }
            None => {
                if b == b'"' || b == b'\'' {
                    quote = Some(b);
                    quote_at = i;
                } else if b == b'|' {
                    // `||` is logical or, not a filter pipe.
                    if bytes.get(i + 1) == Some(&b'|') {
                        i += 2;
                        continue;
                    }
                    scan.last_pipe = Some(i);
                }
            }
        }
        i += 1;
    }
    scan.open_quote = quote.map(|_| quote_at);
    scan
}

fn classify(body: &str) -> Slot<'_> {
    let scan = scan_body(body);
    if let Some(quote_at) = scan.open_quote {
        let partial = &body[quote_at + 1..];
        if first_word(body) == "section" {
            return Slot::SectionName { partial };
        }
        return Slot::StringLiteral;
    }
    if let Some(pipe) = scan.last_pipe {
        return Slot::Filter {
            partial: body[pipe + 1..].trim_start(),
        };
    }
    let trimmed = body.trim_start();
    if !trimmed.contains(char::is_whitespace) {
        return Slot::Leading { partial: trimmed };
    }
    Slot::Expression {
        partial: trailing_identifier(body),
    }
}

fn first_word(body: &str) -> &str {
    body.trim_start()
        .split(|c: char| c.is_whitespace())
        .next()
        .unwrap_or("")
}

/// The identifier the cursor is in the middle of, if any.
fn trailing_identifier(body: &str) -> &str {
    let start = body
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_alphanumeric() || *c == '_')
        .last()
        .map(|(index, _)| index)
        .unwrap_or(body.len());
    &body[start..]
}

fn item(
    label: &str,
    kind: CompletionItemKind,
    detail: String,
    docs: String,
    replace: Range,
) -> CompletionItem {
    CompletionItem {
        label: label.to_string(),
        kind: Some(kind),
        detail: Some(detail),
        documentation: Some(Documentation::MarkupContent(MarkupContent {
            kind: MarkupKind::Markdown,
            value: docs,
        })),
        text_edit: Some(CompletionTextEdit::Edit(TextEdit {
            range: replace,
            new_text: label.to_string(),
        })),
        ..Default::default()
    }
}

fn directive_items(partial: &str, replace: Range) -> Vec<CompletionItem> {
    DIRECTIVES
        .iter()
        .filter(|directive| directive.keyword.starts_with(partial))
        .map(|directive| {
            item(
                directive.keyword,
                CompletionItemKind::KEYWORD,
                directive.syntax.to_string(),
                directive_documentation(directive),
                replace,
            )
        })
        .collect()
}

fn directive_documentation(directive: &Directive) -> String {
    let mut out = format!("`{}`\n\n{}", directive.syntax, directive.summary);
    match directive.role {
        DirectiveRole::Opens {
            closer,
            continuations,
        } => {
            out.push_str(&format!("\n\nCloses with `{{{{ {closer} }}}}`."));
            if !continuations.is_empty() {
                let list = continuations
                    .iter()
                    .map(|k| format!("`{{{{ {k} }}}}`"))
                    .collect::<Vec<_>>()
                    .join(", ");
                out.push_str(&format!(" May contain {list}."));
            }
        }
        DirectiveRole::Continues { opened_by } | DirectiveRole::Closes { opened_by } => {
            let list = opened_by
                .iter()
                .map(|k| format!("`{{{{ {k} }}}}`"))
                .collect::<Vec<_>>()
                .join(" or ");
            let verb = if matches!(directive.role, DirectiveRole::Closes { .. }) {
                "Closes"
            } else {
                "Divides"
            };
            out.push_str(&format!("\n\n{verb} {list}."));
        }
        DirectiveRole::Standalone => {}
    }
    out
}

fn filter_items(partial: &str, replace: Range) -> Vec<CompletionItem> {
    FILTERS
        .iter()
        .filter(|filter| filter.name.starts_with(partial))
        .map(|filter| {
            let mut completion = item(
                filter.name,
                CompletionItemKind::FUNCTION,
                filter.signature(),
                filter_documentation(filter),
                replace,
            );
            // A filter that takes arguments needs the `:` that
            // introduces them, so put the cursor after it.
            if filter.required > 0 {
                completion.text_edit = Some(CompletionTextEdit::Edit(TextEdit {
                    range: replace,
                    new_text: format!("{}: ", filter.name),
                }));
            }
            completion
        })
        .collect()
}

fn filter_documentation(filter: &filters::Filter) -> String {
    format!("`{}`\n\n{}", filter.signature(), filter.summary)
}

fn section_items(partial: &str, replace: Range) -> Vec<CompletionItem> {
    SECTIONS
        .iter()
        .filter(|name| name.starts_with(partial))
        .map(|name| {
            item(
                name,
                CompletionItemKind::ENUM_MEMBER,
                format!("{{{{ section \"{name}\" }}}}"),
                format!(
                    "The `{name}` section. Its envelope is chosen from the active \
                     model's capabilities at render time."
                ),
                replace,
            )
        })
        .collect()
}

/// Names bound by `{{ for }}` blocks around the cursor.
///
/// The template does not tokenize while the directive under the cursor
/// is unterminated, so there is no outline to consult. Dropping that
/// one unfinished directive — text the author has not committed to yet
/// — leaves a document the real parser can read, and the parser reports
/// both the bindings and the byte range they cover. Nothing here models
/// scope.
fn binding_items(
    source: &SourceText,
    open: usize,
    offset: usize,
    partial: &str,
    replace: Range,
) -> Vec<CompletionItem> {
    let mut without_partial = String::with_capacity(source.len());
    without_partial.push_str(&source[..open]);
    without_partial.push_str(&source[offset..]);
    let Ok(blocks) = outline::parse(&without_partial) else {
        return Vec::new();
    };
    // Deleting the unfinished directive only shifts offsets after
    // `open`, so a block that started before it keeps its start, and a
    // block that still ends after it still ends after it. `open` is
    // therefore a valid probe into the repaired coordinates.
    blocks
        .iter()
        .filter(|block| {
            block.kind == OutlineBlockKind::For && block.start <= open && open < block.end
        })
        .flat_map(|block| block.bindings.iter())
        .filter(|name| name.starts_with(partial))
        .map(|name| {
            item(
                name,
                CompletionItemKind::VARIABLE,
                "loop binding".to_string(),
                format!("`{name}` is bound by the enclosing `{{{{ for }}}}`."),
                replace,
            )
        })
        .collect()
}

/// Whether the word starting at `word_start` is the name of a filter —
/// that is, the nearest `|` before it inside this directive is a filter
/// pipe rather than a logical or.
fn preceded_by_filter_pipe(source: &str, body_start: usize, word_start: usize) -> bool {
    if word_start <= body_start {
        return false;
    }
    let before = &source[body_start..word_start];
    let Some(pipe) = scan_body(before).last_pipe else {
        return false;
    };
    // Only whitespace may sit between the pipe and the filter name.
    before[pipe + 1..].trim().is_empty()
}

/// The identifier under the cursor, with its byte range.
fn word_at(source: &str, offset: usize) -> Option<(String, usize, usize)> {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let mut start = offset.min(source.len());
    while start > 0 {
        let prev = source[..start].chars().next_back()?;
        if !is_word(prev) {
            break;
        }
        start -= prev.len_utf8();
    }
    let mut end = offset.min(source.len());
    while let Some(next) = source[end..].chars().next() {
        if !is_word(next) {
            break;
        }
        end += next.len_utf8();
    }
    if start == end {
        return None;
    }
    Some((source[start..end].to_string(), start, end))
}

#[cfg(test)]
mod tests {
    use super::{completions, hover};
    use crate::source_text::SourceText;
    use tower_lsp::lsp_types::{Documentation, HoverContents, MarkupContent, Position};

    /// `‸` marks the cursor. It is stripped before the text reaches the
    /// engine, so a test reads as the buffer the author is looking at.
    fn at_cursor(marked: &str) -> (SourceText, Position) {
        let offset = marked.find('‸').expect("test source needs a ‸ cursor");
        let source = SourceText::new(marked.replace('‸', ""));
        let position = source.position(offset);
        (source, position)
    }

    fn labels(marked: &str) -> Vec<String> {
        let (source, position) = at_cursor(marked);
        let mut out: Vec<String> = completions(&source, position)
            .into_iter()
            .map(|item| item.label)
            .collect();
        out.sort();
        out
    }

    fn detail_of(marked: &str, label: &str) -> String {
        let (source, position) = at_cursor(marked);
        completions(&source, position)
            .into_iter()
            .find(|item| item.label == label)
            .unwrap_or_else(|| panic!("no completion labelled `{label}`"))
            .detail
            .expect("completion should carry a detail")
    }

    fn hover_markdown(marked: &str) -> String {
        let (source, position) = at_cursor(marked);
        match hover(&source, position).expect("expected hover").contents {
            HoverContents::Markup(MarkupContent { value, .. }) => value,
            other => panic!("expected markdown hover, got {other:?}"),
        }
    }

    #[test]
    fn an_empty_directive_offers_the_whole_vocabulary() {
        assert_eq!(
            labels("Hello {{ ‸"),
            vec![
                "elif",
                "else",
                "end",
                "endraw",
                "endsection",
                "for",
                "if",
                "include",
                "raw",
                "section",
            ]
        );
    }

    #[test]
    fn no_end_prefixed_spelling_is_ever_offered() {
        // The point of the whole vocabulary: `{{ endif }}` parses as a
        // variable and renders as literal text, so an editor that
        // suggests it is handing the author a silent bug.
        let offered = labels("{{ if a }}\nx\n{{ e‸");
        assert_eq!(offered, vec!["elif", "else", "end", "endraw", "endsection"]);
        assert!(!offered.iter().any(|label| label == "endif"));
        assert!(!offered.iter().any(|label| label == "endfor"));
    }

    #[test]
    fn openers_say_which_keyword_closes_them() {
        assert!(hover_markdown("{{ i‸f a }}x{{ end }}").contains("Closes with `{{ end }}`"));
        assert!(hover_markdown("{{ f‸or x in xs }}x{{ end }}").contains("Closes with `{{ end }}`"));
        assert!(hover_markdown("{{ section \"task\" }}x{{ e‸ndsection }}")
            .contains("Closes `{{ section }}`"));
        assert!(hover_markdown("{{ r‸aw }}x{{ endraw }}").contains("Closes with `{{ endraw }}`"));
    }

    #[test]
    fn filters_are_offered_after_a_pipe_with_their_signature() {
        assert_eq!(labels("{{ name | up‸"), vec!["upper"]);
        assert_eq!(detail_of("{{ xs | joi‸", "join"), "join: separator");
        assert_eq!(
            detail_of("{{ text | inden‸", "indent"),
            "indent: width[, indent_first]"
        );
    }

    #[test]
    fn a_filter_taking_arguments_inserts_the_colon() {
        let (source, position) = at_cursor("{{ xs | joi‸");
        let item = completions(&source, position)
            .into_iter()
            .find(|item| item.label == "join")
            .expect("join should be offered");
        let edit = format!("{:?}", item.text_edit.expect("text edit"));
        assert!(edit.contains("join: "), "unexpected edit: {edit}");
    }

    #[test]
    fn logical_or_is_not_a_filter_pipe() {
        // `{{ if a || b }}` must not put the cursor in filter position.
        let offered = labels("{{ if a || b‸");
        assert!(
            !offered.iter().any(|label| label == "upper"),
            "filters leaked into an expression: {offered:?}"
        );
    }

    #[test]
    fn section_names_are_offered_inside_the_string() {
        assert_eq!(labels("{{ section \"ta‸"), vec!["task"]);
        assert_eq!(
            labels("{{ section \"‸"),
            vec![
                "chain_of_thought",
                "examples",
                "output_format",
                "system_framing",
                "task",
                "thinking_scaffold",
                "tools",
            ]
        );
    }

    #[test]
    fn other_string_literals_offer_nothing() {
        assert!(labels("{{ include \"partial‸").is_empty());
    }

    #[test]
    fn loop_bindings_are_offered_inside_the_loop() {
        assert!(labels("{{ for item in items }}\n- {{ it‸ }}\n{{ end }}\n")
            .contains(&"item".to_string()));
        let both = labels("{{ for key, value in dict }}\n{{ ‸ }}\n{{ end }}\n");
        assert!(both.contains(&"key".to_string()), "got {both:?}");
        assert!(both.contains(&"value".to_string()), "got {both:?}");
    }

    #[test]
    fn loop_bindings_are_not_offered_outside_the_loop() {
        let outside = labels("{{ for item in items }}x{{ end }}\n{{ it‸ }}\n");
        assert!(
            !outside.contains(&"item".to_string()),
            "`item` is out of scope here: {outside:?}"
        );
    }

    #[test]
    fn nested_loops_offer_every_enclosing_binding() {
        let offered = labels(concat!(
            "{{ for outer in groups }}\n",
            "{{ for inner in outer }}\n",
            "{{ ‸ }}\n",
            "{{ end }}\n",
            "{{ end }}\n",
        ));
        assert!(offered.contains(&"outer".to_string()), "got {offered:?}");
        assert!(offered.contains(&"inner".to_string()), "got {offered:?}");
    }

    #[test]
    fn template_prose_offers_nothing() {
        assert!(labels("You are a helpful ‸assistant.").is_empty());
        assert!(labels("{{ name }} then ‸later").is_empty());
    }

    #[test]
    fn comments_and_raw_blocks_offer_nothing() {
        assert!(labels("{{# a note ‸").is_empty());
        assert!(labels("{{ raw }}\n{{ up‸ }}\n{{ endraw }}\n").is_empty());
    }

    #[test]
    fn hover_explains_a_filter() {
        let markdown = hover_markdown("{{ name | up‸per }}");
        assert!(markdown.contains("upper"));
        assert!(markdown.contains("Uppercase"), "got {markdown}");
    }

    #[test]
    fn hover_on_prose_is_silent() {
        let (source, position) = at_cursor("You are a helpful assist‸ant.");
        assert!(hover(&source, position).is_none());
    }

    #[test]
    fn completion_documentation_is_markdown() {
        let (source, position) = at_cursor("{{ i‸");
        let item = completions(&source, position)
            .into_iter()
            .find(|item| item.label == "if")
            .expect("if should be offered");
        match item.documentation.expect("documentation") {
            Documentation::MarkupContent(MarkupContent { value, .. }) => {
                assert!(value.contains("Closes with `{{ end }}`"), "got {value}");
            }
            other => panic!("expected markup documentation, got {other:?}"),
        }
    }
}
