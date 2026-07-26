//! Document formatting and code-action quick-fixes.

use std::collections::HashMap;

use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;

use crate::helpers::{
    code_action_kind_for_safety_name, diagnostic_repair_code_action_data,
    diagnostic_repair_code_action_kind, extract_backtick_name, find_word_in_region,
    repair_code_action_data, repair_code_action_kind, span_to_range,
};
use crate::rules::RuleDiagnostic;
use crate::source_text::SourceText;
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
                Some(s) if s.kind.is_harn() => s.source.clone(),
                _ => return Ok(None),
            }
        };

        Ok(format_whole_document_edit(&source).map(|edit| vec![edit]))
    }

    pub(super) async fn handle_range_formatting(
        &self,
        params: DocumentRangeFormattingParams,
    ) -> Result<Option<Vec<TextEdit>>> {
        let uri = &params.text_document.uri;
        let source = {
            let docs = self.documents.lock().unwrap();
            match docs.get(uri) {
                Some(s) if s.kind.is_harn() => s.source.clone(),
                _ => return Ok(None),
            }
        };

        Ok(range_format_edit(&source, params.range).map(|edit| vec![edit]))
    }

    pub(super) async fn handle_on_type_formatting(
        &self,
        params: DocumentOnTypeFormattingParams,
    ) -> Result<Option<Vec<TextEdit>>> {
        if params.ch != ";" && params.ch != "}" {
            return Ok(None);
        }

        let uri = &params.text_document_position.text_document.uri;
        let source = {
            let docs = self.documents.lock().unwrap();
            match docs.get(uri) {
                Some(s) if s.kind.is_harn() => s.source.clone(),
                _ => return Ok(None),
            }
        };

        Ok(format_whole_document_edit(&source).map(|edit| vec![edit]))
    }

    pub(super) async fn handle_code_action(
        &self,
        params: CodeActionParams,
    ) -> Result<Option<CodeActionResponse>> {
        let uri = &params.text_document.uri;

        let (source, lint_diags, type_diags, rule_diags) = {
            let docs = self.documents.lock().unwrap();
            let state = match docs.get(uri) {
                Some(s) => s,
                None => return Ok(Some(Vec::new())),
            };
            (
                state.source.clone(),
                state.lint_diagnostics.clone(),
                state.type_diagnostics.clone(),
                state.rule_diagnostics.clone(),
            )
        };

        let actions = build_code_actions(
            uri,
            &source,
            &lint_diags,
            &type_diags,
            &rule_diags,
            &params.context,
        );

        Ok(Some(actions))
    }
}

pub(crate) fn build_code_actions(
    uri: &Url,
    source: &SourceText,
    lint_diags: &[harn_lint::LintDiagnostic],
    type_diags: &[harn_parser::TypeDiagnostic],
    rule_diags: &[RuleDiagnostic],
    context: &CodeActionContext,
) -> CodeActionResponse {
    let mut actions = Vec::new();

    for diag in &context.diagnostics {
        if let Some(rule_diag) = matching_rule_diagnostic(rule_diags, diag) {
            if let (Some(text_edit), Some(repair_id)) = (&rule_diag.edit, &rule_diag.repair_id) {
                let safety = diag
                    .data
                    .as_ref()
                    .and_then(|data| data.get("safety"))
                    .and_then(|value| value.as_str())
                    .unwrap_or("scope-local");
                let mut changes = HashMap::new();
                changes.insert(uri.clone(), vec![text_edit.clone()]);
                actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                    title: rule_diag.title.clone(),
                    kind: Some(code_action_kind_for_safety_name(safety)),
                    diagnostics: Some(vec![diag.clone()]),
                    data: Some(serde_json::json!({
                        "repair_id": repair_id,
                        "safety": safety,
                        "diagnostic_code": crate::helpers::diagnostic_code_string(diag.code.as_ref()),
                    })),
                    edit: Some(WorkspaceEdit {
                        changes: Some(changes),
                        ..Default::default()
                    }),
                    ..Default::default()
                }));
                continue;
            }
        }

        let msg = &diag.message;

        if let Some(ld) = lint_diags.iter().find(|ld| {
            msg.contains(&format!("[{}]", ld.rule)) && span_to_range(&ld.span) == diag.range
        }) {
            if let Some(ref fix_edits) = ld.fix {
                let repair = ld.repair();
                let text_edits: Vec<TextEdit> = fix_edits
                    .iter()
                    .map(|fe| TextEdit {
                        range: Range {
                            start: source.position(fe.span.start),
                            end: source.position(fe.span.end),
                        },
                        new_text: fe.replacement.clone(),
                    })
                    .collect();

                let title = match ld.rule.as_ref() {
                    "mutable-never-reassigned" => "Change `var` to `let`".to_string(),
                    "comparison-to-bool" => "Simplify boolean comparison".to_string(),
                    "unnecessary-else-return" => "Remove unnecessary else".to_string(),
                    "unused-import" => {
                        let name = extract_backtick_name(msg).unwrap_or_else(|| "name".to_string());
                        format!("Remove unused import `{name}`")
                    }
                    "invalid-binary-op-literal" => "Convert to string interpolation".to_string(),
                    "unnecessary-cast" => "Remove unnecessary cast".to_string(),
                    _ => ld
                        .suggestion
                        .clone()
                        .unwrap_or_else(|| "Apply fix".to_string()),
                };

                let mut changes = HashMap::new();
                changes.insert(uri.clone(), text_edits);
                actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                    title,
                    kind: Some(repair_code_action_kind(repair.as_ref())),
                    diagnostics: Some(vec![diag.clone()]),
                    data: repair_code_action_data(diag, repair.as_ref()),
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
                                start: source.position(fe.span.start),
                                end: source.position(fe.span.end),
                            },
                            new_text: fe.replacement.clone(),
                        })
                        .collect();

                    let mut changes = HashMap::new();
                    changes.insert(uri.clone(), text_edits);
                    actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                        title: "Convert to string interpolation".to_string(),
                        kind: Some(repair_code_action_kind(td.repair.as_ref())),
                        diagnostics: Some(vec![diag.clone()]),
                        data: repair_code_action_data(diag, td.repair.as_ref()),
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
                    if let Some(edit) = build_missing_arms_edit(source, span, missing) {
                        let mut changes = HashMap::new();
                        changes.insert(uri.clone(), vec![edit]);
                        actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                            title: if missing.len() == 1 {
                                format!("Add missing match arm {}", missing[0])
                            } else {
                                format!("Add missing match arms ({})", missing.len())
                            },
                            kind: Some(repair_code_action_kind(td.repair.as_ref())),
                            diagnostics: Some(vec![diag.clone()]),
                            data: repair_code_action_data(diag, td.repair.as_ref()),
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
                let offset = source.offset(diag.range.start);
                let end_offset = source
                    .offset(diag.range.end)
                    .max(offset + 1)
                    .min(source.len());
                let search_region = &source[offset..end_offset];
                if let Some(name_pos) = find_word_in_region(search_region, &name) {
                    let abs_pos = offset + name_pos;
                    let start = source.position(abs_pos);
                    let end = source.position(abs_pos + name.len());
                    let edit_range = Range { start, end };

                    let mut changes = HashMap::new();
                    changes.insert(
                        uri.clone(),
                        vec![TextEdit {
                            range: edit_range,
                            new_text: "_".to_string(),
                        }],
                    );
                    let label = if msg.contains("[unused-variable]") {
                        "variable"
                    } else {
                        "parameter"
                    };
                    actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                        title: format!("Replace unused {label} `{name}` with `_`"),
                        kind: Some(diagnostic_repair_code_action_kind(diag)),
                        diagnostics: Some(vec![diag.clone()]),
                        data: diagnostic_repair_code_action_data(diag),
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

    if fix_all_requested(context.only.as_deref()) {
        let mut all_edits: Vec<harn_lexer::FixEdit> = Vec::new();
        for ld in lint_diags {
            if let Some(fix) = &ld.fix {
                all_edits.extend(fix.iter().cloned());
            }
        }
        for td in type_diags {
            if let Some(fix) = &td.fix {
                all_edits.extend(fix.iter().cloned());
            }
        }
        // Drop overlaps via the shared policy so the on-save path produces
        // byte-for-byte what `harn lint --fix` would.
        let accepted = harn_lexer::FixEdit::dedupe_overlapping(&all_edits);
        if !accepted.is_empty() {
            let text_edits: Vec<TextEdit> = accepted
                .iter()
                .map(|fe| TextEdit {
                    range: Range {
                        start: source.position(fe.span.start),
                        end: source.position(fe.span.end),
                    },
                    new_text: fe.replacement.clone(),
                })
                .collect();
            let mut changes = HashMap::new();
            changes.insert(uri.clone(), text_edits);
            actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                title: "Apply all Harn autofixes".to_string(),
                kind: Some(CodeActionKind::new("source.fixAll.harn")),
                edit: Some(WorkspaceEdit {
                    changes: Some(changes),
                    ..Default::default()
                }),
                ..Default::default()
            }));
        }
    }

    actions
}

fn matching_rule_diagnostic<'a>(
    rule_diags: &'a [RuleDiagnostic],
    diagnostic: &Diagnostic,
) -> Option<&'a RuleDiagnostic> {
    let repair_id = diagnostic
        .data
        .as_ref()
        .and_then(|data| data.get("repair_id"))
        .and_then(|value| value.as_str());
    rule_diags.iter().find(|item| {
        item.diagnostic.range == diagnostic.range
            && item.diagnostic.message == diagnostic.message
            && item.repair_id.as_deref() == repair_id
    })
}

fn format_whole_document_edit(source: &str) -> Option<TextEdit> {
    let formatted = harn_fmt::format_source(source).ok()?;
    if formatted == source {
        return None;
    }

    let line_count = source.lines().count() as u32;
    let last_line_len = source.lines().last().map_or(0, |l| l.len()) as u32;
    Some(TextEdit {
        range: Range {
            start: Position::new(0, 0),
            end: Position::new(line_count, last_line_len),
        },
        new_text: formatted,
    })
}

/// Compute a "format selection" edit that confines its changes to the
/// requested `range`, reusing the whole-document formatter.
///
/// Harn's formatter (`harn_fmt::format_source`) only formats a complete
/// program, so there is no native partial formatter to call. Instead we
/// format the whole document, trim the common leading/trailing lines to
/// isolate the region the formatter actually changed, and emit an edit
/// for that region **only when it lies entirely within the selected
/// lines**. If the formatter's changes spill outside the selection we
/// return `None` rather than reformat code the user didn't select — this
/// keeps "Format Selection" from silently rewriting the whole file while
/// still handling the common cases (a whole-file selection, or a
/// selection that fully contains the messy region).
fn range_format_edit(source: &SourceText, range: Range) -> Option<TextEdit> {
    let formatted = harn_fmt::format_source(source).ok()?;
    if formatted == source.as_str() {
        return None;
    }

    let orig: Vec<&str> = source.split('\n').collect();
    let fmt: Vec<&str> = formatted.split('\n').collect();

    // Longest common line-prefix and line-suffix, without overlapping.
    let mut prefix = 0;
    while prefix < orig.len() && prefix < fmt.len() && orig[prefix] == fmt[prefix] {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < orig.len() - prefix
        && suffix < fmt.len() - prefix
        && orig[orig.len() - 1 - suffix] == fmt[fmt.len() - 1 - suffix]
    {
        suffix += 1;
    }

    // Original lines `[prefix, orig.len() - suffix)` are what changed; the
    // replacement is formatted lines `[prefix, fmt.len() - suffix)`.
    let orig_end_line = orig.len() - suffix;

    // The last document line the selection covers. A selection that ends at
    // column 0 of a line does not actually include that line's text, so we
    // drop it (standard editor convention).
    let start_line = range.start.line as usize;
    let last_selected_line = if range.end.character == 0 && range.end.line > range.start.line {
        (range.end.line - 1) as usize
    } else {
        range.end.line as usize
    };

    // Require the changed region's lines to sit within the selection.
    if prefix < start_line || orig_end_line > last_selected_line + 1 {
        return None;
    }

    let start_offset = source.offset(Position::new(prefix as u32, 0));
    let new_lines = &fmt[prefix..fmt.len() - suffix];
    let (end_offset, new_text) = if orig_end_line < orig.len() {
        // The region ends before the final line, so it is replaced up to the
        // start of the next line — include each replacement line's newline.
        let end = source.offset(Position::new(orig_end_line as u32, 0));
        let mut text = new_lines.join("\n");
        if !new_lines.is_empty() {
            text.push('\n');
        }
        (end, text)
    } else {
        // The region runs to the end of the document (no trailing newline to
        // account for).
        (source.len(), new_lines.join("\n"))
    };

    Some(TextEdit {
        range: Range {
            start: source.position(start_offset),
            end: source.position(end_offset),
        },
        new_text,
    })
}

/// Returns `true` when the editor's `CodeActionContext.only` filter
/// explicitly asks for a fix-all kind. We deliberately do NOT opt in
/// when `only` is `None` so the bulk action does not pollute the regular
/// Cmd+. menu — it should only fire from `editor.codeActionsOnSave`
/// (which sends `["source.fixAll"]` or `["source.fixAll.harn"]`) or the
/// "Source Action…" command (which sends `["source"]`).
fn fix_all_requested(only: Option<&[CodeActionKind]>) -> bool {
    let Some(kinds) = only else {
        return false;
    };
    kinds.iter().any(|k| {
        let s = k.as_str();
        s == "source.fixAll" || s == "source.fixAll.harn" || s == "source"
    })
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
    source: &SourceText,
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
    let brace_pos = source.position(close_brace_byte);
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
    use super::{
        build_code_actions, build_missing_arms_edit, format_whole_document_edit, range_format_edit,
    };
    use crate::document::DocumentState;
    use crate::source_text::SourceText;
    use harn_lexer::Span;
    use tower_lsp::lsp_types::{
        CodeActionContext, CodeActionOrCommand, NumberOrString, Position, Range, Url,
    };

    #[test]
    fn repair_quickfix_actions_carry_safety_kind_and_flat_data() {
        let source =
            "pipeline main() { const count = 1; const greeting = \"hello \" + count; greeting }\n";
        let state = DocumentState::new(source.to_string());
        let diagnostic = state
            .diagnostics
            .iter()
            .find(|diagnostic| {
                matches!(
                    diagnostic.code.as_ref(),
                    Some(NumberOrString::String(code)) if code == "HARN-TYP-003"
                )
            })
            .expect("expected string-interpolation repair diagnostic")
            .clone();
        let uri = Url::parse("file:///workspace/main.harn").unwrap();
        let context = CodeActionContext {
            diagnostics: vec![diagnostic],
            only: None,
            trigger_kind: None,
        };

        let actions = build_code_actions(
            &uri,
            &state.source,
            &state.lint_diagnostics,
            &state.type_diagnostics,
            &state.rule_diagnostics,
            &context,
        );

        let action = actions
            .into_iter()
            .find_map(|action| match action {
                CodeActionOrCommand::CodeAction(action) => Some(action),
                CodeActionOrCommand::Command(_) => None,
            })
            .expect("expected a code action");
        assert_eq!(
            action.kind.as_ref().map(|kind| kind.as_str()),
            Some("quickfix.harn.behavior-preserving")
        );
        let data = action.data.as_ref().expect("repair action data");
        assert_eq!(
            data.get("repair_id").and_then(|value| value.as_str()),
            Some("style/string-interpolation")
        );
        assert_eq!(
            data.get("safety").and_then(|value| value.as_str()),
            Some("behavior-preserving")
        );
        assert_eq!(
            data.get("diagnostic_code").and_then(|value| value.as_str()),
            Some("HARN-TYP-003")
        );
    }

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
        let edit = build_missing_arms_edit(&SourceText::new(source), &span, &missing)
            .expect("expected edit for well-formed match");
        assert!(edit.new_text.contains("\"fail\" -> "), "{edit:?}");
        assert!(edit.new_text.contains("\"skip\" -> "), "{edit:?}");
        assert!(
            edit.new_text.contains("unreachable"),
            "edit should scaffold with unreachable: {edit:?}"
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
        let edit = build_missing_arms_edit(&SourceText::new(source), &span, &["\"x\"".to_string()]);
        assert!(edit.is_none());
    }

    #[test]
    fn range_format_selecting_whole_document_formats_everything() {
        let source = "fn main(){\nconst x=1\n}\n";
        // Selection spanning the entire document.
        let edit = range_format_edit(
            &SourceText::new(source),
            Range::new(Position::new(0, 0), Position::new(3, 0)),
        )
        .expect("expected an edit for a messy document");
        assert!(edit.new_text.contains("fn main() {"), "{}", edit.new_text);
        assert!(edit.new_text.contains("const x = 1"), "{}", edit.new_text);
        assert_eq!(edit.range.start, Position::new(0, 0));
    }

    #[test]
    fn range_format_confines_edit_to_selected_lines() {
        let source = "fn main(){\nconst x=1\n}\n";
        // Selection covering just the two messy lines (0 and 1).
        let edit = range_format_edit(
            &SourceText::new(source),
            Range::new(Position::new(0, 0), Position::new(2, 0)),
        )
        .expect("expected an edit for the selected messy region");
        // The edit must not reach past line 1 into the closing brace on line 2.
        assert!(
            edit.range.end.line <= 2,
            "edit should stay within the selection, got {:?}",
            edit.range
        );
        assert!(edit.new_text.contains("const x = 1"), "{}", edit.new_text);
    }

    #[test]
    fn range_format_returns_none_when_selection_misses_changes() {
        let source = "fn main(){\nconst x=1\n}\n";
        // Selecting only the already-well-formatted closing brace line: the
        // formatter's changes lie outside the selection, so no edit.
        let edit = range_format_edit(
            &SourceText::new(source),
            Range::new(Position::new(2, 0), Position::new(3, 0)),
        );
        assert!(
            edit.is_none(),
            "expected no edit when the selection excludes the changed region: {edit:?}"
        );
    }

    #[test]
    fn range_format_returns_none_for_already_formatted_source() {
        let source = "fn main() {\n  const x = 1\n}\n";
        let edit = range_format_edit(
            &SourceText::new(source),
            Range::new(Position::new(0, 0), Position::new(3, 0)),
        );
        assert!(edit.is_none(), "already-formatted source needs no edit");
    }

    #[test]
    fn whole_document_format_edit_reuses_formatter_for_on_type_formatting() {
        let source = "fn main(){\nconst x=1;\n}\n";
        let edit = format_whole_document_edit(source).expect("expected formatting edit");
        assert!(edit.new_text.contains("fn main() {"), "{}", edit.new_text);
        assert!(edit.new_text.contains("const x = 1"), "{}", edit.new_text);
    }
}
