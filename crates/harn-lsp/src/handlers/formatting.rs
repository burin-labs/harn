//! Document formatting and code-action quick-fixes.

use std::collections::HashMap;

use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;

use crate::helpers::{
    extract_backtick_name, find_word_in_region, lsp_position_to_offset, offset_to_position,
    span_to_range,
};
use crate::HarnLsp;

impl HarnLsp {
    pub(super) async fn handle_formatting(
        &self,
        params: DocumentFormattingParams,
    ) -> Result<Option<Vec<TextEdit>>> {
        let uri = &params.text_document.uri;
        let source = {
            let docs = self.documents.lock().unwrap();
            match docs.get(uri) {
                Some(s) => s.source.clone(),
                None => return Ok(None),
            }
        };

        let formatted = match harn_fmt::format_source(&source) {
            Ok(f) => f,
            Err(_) => return Ok(None),
        };

        if formatted == source {
            return Ok(None);
        }

        let line_count = source.lines().count() as u32;
        let last_line_len = source.lines().last().map_or(0, |l| l.len()) as u32;
        Ok(Some(vec![TextEdit {
            range: Range {
                start: Position::new(0, 0),
                end: Position::new(line_count, last_line_len),
            },
            new_text: formatted,
        }]))
    }

    pub(super) async fn handle_code_action(
        &self,
        params: CodeActionParams,
    ) -> Result<Option<CodeActionResponse>> {
        let uri = &params.text_document.uri;
        let mut actions = Vec::new();

        let (source, lint_diags, type_diags) = {
            let docs = self.documents.lock().unwrap();
            let state = match docs.get(uri) {
                Some(s) => s,
                None => return Ok(Some(actions)),
            };
            (
                state.source.clone(),
                state.lint_diagnostics.clone(),
                state.type_diagnostics.clone(),
            )
        };

        for diag in &params.context.diagnostics {
            let msg = &diag.message;

            if let Some(ld) = lint_diags.iter().find(|ld| {
                msg.contains(&format!("[{}]", ld.rule)) && span_to_range(&ld.span) == diag.range
            }) {
                if let Some(ref fix_edits) = ld.fix {
                    let text_edits: Vec<TextEdit> = fix_edits
                        .iter()
                        .map(|fe| TextEdit {
                            range: Range {
                                start: offset_to_position(&source, fe.span.start),
                                end: offset_to_position(&source, fe.span.end),
                            },
                            new_text: fe.replacement.clone(),
                        })
                        .collect();

                    let title = match ld.rule {
                        "mutable-never-reassigned" => "Change `var` to `let`".to_string(),
                        "comparison-to-bool" => "Simplify boolean comparison".to_string(),
                        "unnecessary-else-return" => "Remove unnecessary else".to_string(),
                        "unused-import" => {
                            let name =
                                extract_backtick_name(msg).unwrap_or_else(|| "name".to_string());
                            format!("Remove unused import `{name}`")
                        }
                        "invalid-binary-op-literal" => {
                            "Convert to string interpolation".to_string()
                        }
                        _ => ld
                            .suggestion
                            .clone()
                            .unwrap_or_else(|| "Apply fix".to_string()),
                    };

                    let mut changes = HashMap::new();
                    changes.insert(uri.clone(), text_edits);
                    actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                        title,
                        kind: Some(CodeActionKind::QUICKFIX),
                        diagnostics: Some(vec![diag.clone()]),
                        edit: Some(WorkspaceEdit {
                            changes: Some(changes),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }));
                    continue;
                }
            }

            if diag.source.as_deref() == Some("harn-typecheck") {
                if let Some(td) = type_diags.iter().find(|td| {
                    td.message == *msg && td.span.as_ref().map(span_to_range) == Some(diag.range)
                }) {
                    if let Some(ref fix_edits) = td.fix {
                        let text_edits: Vec<TextEdit> = fix_edits
                            .iter()
                            .map(|fe| TextEdit {
                                range: Range {
                                    start: offset_to_position(&source, fe.span.start),
                                    end: offset_to_position(&source, fe.span.end),
                                },
                                new_text: fe.replacement.clone(),
                            })
                            .collect();

                        let mut changes = HashMap::new();
                        changes.insert(uri.clone(), text_edits);
                        actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                            title: "Convert to string interpolation".to_string(),
                            kind: Some(CodeActionKind::QUICKFIX),
                            diagnostics: Some(vec![diag.clone()]),
                            edit: Some(WorkspaceEdit {
                                changes: Some(changes),
                                ..Default::default()
                            }),
                            ..Default::default()
                        }));
                        continue;
                    }

                    // Non-exhaustive match: synthesise an "Add missing
                    // arms" quick-fix from the structured details on
                    // the diagnostic. The diagnostic's span covers the
                    // whole `match` expression, so the closing `}`
                    // sits at `span.end - 1`. We insert `arm_indent`
                    // + pattern + `-> { unreachable(...) }` right
                    // before the `}`, using the closing brace's
                    // column as the reference indent.
                    if let (
                        Some(harn_parser::DiagnosticDetails::NonExhaustiveMatch { missing }),
                        Some(span),
                    ) = (td.details.as_ref(), td.span.as_ref())
                    {
                        if let Some(edit) = build_missing_arms_edit(&source, span, missing) {
                            let mut changes = HashMap::new();
                            changes.insert(uri.clone(), vec![edit]);
                            actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                                title: if missing.len() == 1 {
                                    format!("Add missing match arm {}", missing[0])
                                } else {
                                    format!("Add missing match arms ({})", missing.len())
                                },
                                kind: Some(CodeActionKind::QUICKFIX),
                                diagnostics: Some(vec![diag.clone()]),
                                edit: Some(WorkspaceEdit {
                                    changes: Some(changes),
                                    ..Default::default()
                                }),
                                is_preferred: Some(true),
                                ..Default::default()
                            }));
                            continue;
                        }
                    }
                }
            }

            // Fallback manual code actions for rules without structured fixes.
            if msg.contains("[unused-variable]") || msg.contains("[unused-parameter]") {
                if let Some(name) = extract_backtick_name(msg) {
                    let offset = lsp_position_to_offset(&source, diag.range.start);
                    let end_offset = lsp_position_to_offset(&source, diag.range.end)
                        .max(offset + 1)
                        .min(source.len());
                    let search_region = &source[offset..end_offset];
                    if let Some(name_pos) = find_word_in_region(search_region, &name) {
                        let abs_pos = offset + name_pos;
                        let start = offset_to_position(&source, abs_pos);
                        let end = offset_to_position(&source, abs_pos + name.len());
                        let edit_range = Range { start, end };

                        let mut changes = HashMap::new();
                        changes.insert(
                            uri.clone(),
                            vec![TextEdit {
                                range: edit_range,
                                new_text: format!("_{name}"),
                            }],
                        );
                        let label = if msg.contains("[unused-variable]") {
                            "variable"
                        } else {
                            "parameter"
                        };
                        actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                            title: format!("Prefix {label} `{name}` with `_`"),
                            kind: Some(CodeActionKind::QUICKFIX),
                            diagnostics: Some(vec![diag.clone()]),
                            edit: Some(WorkspaceEdit {
                                changes: Some(changes),
                                ..Default::default()
                            }),
                            ..Default::default()
                        }));
                    }
                }
            }
        }

        Ok(Some(actions))
    }
}

/// Build a `TextEdit` that inserts "missing" match arms just before
/// the `}` that closes the match expression at `match_span`. Each
/// missing variant becomes one new arm of the form
/// `{pattern} -> { unreachable("TODO: handle {pattern}") }`, indented
/// relative to the closing brace.
///
/// Returns `None` when the span doesn't look like a well-formed
/// `match` expression (e.g. the closing `}` isn't at the expected
/// byte position) — in that case the code-action is silently skipped
/// rather than emitting a broken edit.
pub(super) fn build_missing_arms_edit(
    source: &str,
    match_span: &harn_lexer::Span,
    missing: &[String],
) -> Option<TextEdit> {
    if missing.is_empty() {
        return None;
    }
    // Span.end is exclusive: the last byte of the match — the `}` —
    // is at span.end - 1.
    let close_brace_byte = match_span.end.checked_sub(1)?;
    let bytes = source.as_bytes();
    if close_brace_byte >= bytes.len() || bytes[close_brace_byte] != b'}' {
        return None;
    }
    // Measure the closing brace's indent by walking back from its
    // position to the start of its line and counting whitespace.
    let line_start = source[..close_brace_byte]
        .rfind('\n')
        .map(|n| n + 1)
        .unwrap_or(0);
    let indent_slice = &source[line_start..close_brace_byte];
    let brace_indent: String = indent_slice
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect();
    // Arm indent is brace indent + 2 spaces (Harn formatter
    // convention). If the brace is on the same line as other content
    // (e.g. a single-line match), `indent_slice` still starts with
    // whatever lead-in was there — we conservatively still add 2
    // spaces of nesting, which produces correct but possibly ugly
    // output on single-line matches.
    let arm_indent = format!("{brace_indent}  ");
    let mut inserted = String::new();
    for pattern in missing {
        inserted.push('\n');
        inserted.push_str(&arm_indent);
        inserted.push_str(pattern);
        inserted.push_str(" -> { unreachable(\"TODO: handle ");
        inserted.push_str(pattern);
        inserted.push_str("\") }");
    }
    inserted.push('\n');
    inserted.push_str(&brace_indent);
    let brace_pos = offset_to_position(source, close_brace_byte);
    Some(TextEdit {
        range: Range {
            start: brace_pos,
            end: brace_pos,
        },
        new_text: inserted,
    })
}

#[cfg(test)]
mod tests {
    use super::build_missing_arms_edit;
    use harn_lexer::Span;

    #[test]
    fn missing_arms_edit_inserts_each_variant_before_close_brace() {
        let source = "pipeline default() {\n  match v {\n    \"pass\" -> { }\n  }\n}\n";
        // Byte range covering `match v { ... }`.
        let start = source.find("match").unwrap();
        let end = source[start..].find('\n').unwrap();
        let match_block_start = start;
        let match_block_end_brace = source
            .match_indices('\n')
            .filter(|(idx, _)| *idx > start)
            .nth(2)
            .map(|(idx, _)| idx)
            .unwrap();
        // Find the actual `}` that closes the match block.
        let close_brace_pos = source[match_block_start..match_block_end_brace]
            .rfind('}')
            .map(|r| match_block_start + r)
            .unwrap();
        let span = Span {
            start: match_block_start,
            end: close_brace_pos + 1,
            line: 2,
            column: 3,
            end_line: 4,
        };
        let missing = vec!["\"fail\"".to_string(), "\"skip\"".to_string()];
        let _ = end;
        let edit = build_missing_arms_edit(source, &span, &missing)
            .expect("expected edit for well-formed match");
        assert!(edit.new_text.contains("\"fail\" -> "), "{:?}", edit);
        assert!(edit.new_text.contains("\"skip\" -> "), "{:?}", edit);
        assert!(
            edit.new_text.contains("unreachable"),
            "edit should scaffold with unreachable: {:?}",
            edit
        );
        // Indent should be 4 spaces for arms (brace at col 2 + 2).
        assert!(
            edit.new_text.contains("\n    \"fail\""),
            "expected 4-space arm indent, got: {:?}",
            edit.new_text
        );
    }

    #[test]
    fn missing_arms_edit_returns_none_when_close_brace_missing() {
        let source = "not a match expression";
        let span = Span {
            start: 0,
            end: source.len(),
            line: 1,
            column: 1,
            end_line: 1,
        };
        let edit = build_missing_arms_edit(source, &span, &["\"x\"".to_string()]);
        assert!(edit.is_none());
    }
}
