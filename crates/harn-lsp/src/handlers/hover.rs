//! Hover, signature help, and inlay hints.

use harn_parser::format_type;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;

use crate::constants::{builtin_doc, builtin_signature, keyword_doc};
use crate::helpers::{is_member_access, word_span_at_position};
use crate::source_text::SourceText;
use crate::symbols::{
    format_flow_attributes_block, format_shape_expanded, format_union_shapes_expanded,
    HarnSymbolKind, SymbolInfo,
};
use crate::HarnLsp;

/// What the word under the cursor resolves to.
///
/// Resolution is separated from rendering so hover tests drive the order in
/// which the handler consults builtins, keywords and symbols, rather than a
/// copy of it.
#[derive(Debug, Clone)]
pub(crate) enum HoverTarget {
    Builtin(String),
    Keyword(String),
    Symbol(Box<SymbolInfo>),
}

/// Resolve the word under the cursor. `None` means nothing is known about it,
/// which is the honest answer for an unresolved member access.
pub(crate) fn resolve_hover_target(
    source: &SourceText,
    symbols: &[SymbolInfo],
    position: Position,
) -> Option<HoverTarget> {
    let (word, word_start) = word_span_at_position(source, position)?;

    // A member access names something on its receiver, so no global namespace
    // applies to it: not builtins, not keywords, and not top-level bindings. A
    // member that resolves to a global describes something the receiver does not
    // have, which makes a call the runtime rejects look implemented.
    let member_access = is_member_access(source, word_start);

    if !member_access {
        if let Some(doc) = builtin_doc(&word) {
            return Some(HoverTarget::Builtin(doc));
        }
        if let Some(doc) = keyword_doc(&word) {
            return Some(HoverTarget::Keyword(doc));
        }
    }

    // Prefer the innermost scope that contains the cursor position so that
    // shadowed bindings resolve to the closest definition.
    let cursor_offset = source.offset(position);
    let mut best: Option<&SymbolInfo> = None;
    for sym in symbols {
        if sym.name != word {
            continue;
        }
        // Only a receiver-owned symbol can answer a member access. A top-level
        // binding carries no scope span and would otherwise pass the scope check
        // below, so `value.greet` would resolve to a global `fn greet`. Until
        // receiver types are inferred, an impl-block method is the only symbol
        // that can be receiver-owned; anything else is a name collision.
        if member_access && sym.impl_type.is_none() {
            continue;
        }
        // Impl-block methods are visible through dot syntax from anywhere, so a
        // cursor-position scope check does not apply to them.
        let in_scope = if sym.impl_type.is_some() {
            true
        } else {
            match sym.scope_span {
                Some(sp) => cursor_offset >= sp.start && cursor_offset <= sp.end,
                None => true,
            }
        };
        if !in_scope {
            continue;
        }
        // Tightest-scope wins on shadowing.
        match best {
            None => best = Some(sym),
            Some(prev) => {
                let prev_scope_size = match prev.scope_span {
                    Some(sp) => sp.end.saturating_sub(sp.start),
                    None => usize::MAX,
                };
                let this_scope_size = match sym.scope_span {
                    Some(sp) => sp.end.saturating_sub(sp.start),
                    None => usize::MAX,
                };
                if this_scope_size < prev_scope_size {
                    best = Some(sym);
                }
            }
        }
    }

    best.map(|sym| HoverTarget::Symbol(Box::new(sym.clone())))
}

fn line_prefix_at_position(source: &SourceText, position: Position) -> Option<&str> {
    let offset = source.offset(position);
    if offset == source.len() && source.position(offset).line < position.line {
        return None;
    }
    let line_start = source[..offset]
        .rfind('\n')
        .map(|idx| idx + '\n'.len_utf8())
        .unwrap_or(0);
    Some(&source[line_start..offset])
}

impl HarnLsp {
    pub(super) async fn handle_hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;

        let docs = self.documents.lock().unwrap();
        let state = match docs.get(uri) {
            Some(s) => s,
            None => return Ok(None),
        };
        if !state.kind.is_harn() {
            return Ok(None);
        }
        let source = state.source.clone();
        let symbols = state.symbols.clone();
        drop(docs);

        let markup = |value: String| {
            Ok(Some(Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value,
                }),
                range: None,
            }))
        };

        let sym = match resolve_hover_target(&source, &symbols, position) {
            None => return Ok(None),
            Some(HoverTarget::Builtin(doc)) | Some(HoverTarget::Keyword(doc)) => {
                return markup(doc)
            }
            Some(HoverTarget::Symbol(sym)) => sym,
        };

        {
            let sym = sym.as_ref();
            let mut hover_text = String::new();

            if let Some(ref sig) = sym.signature {
                let display_sig = if let Some(ref impl_ty) = sym.impl_type {
                    format!("impl {impl_ty}\n{sig}")
                } else {
                    sig.clone()
                };
                hover_text.push_str(&format!("```harn\n{display_sig}\n```\n"));
            } else {
                let keyword = match sym.kind {
                    HarnSymbolKind::Variable => "let",
                    HarnSymbolKind::Parameter => "param",
                    _ => "",
                };
                if let Some(ref ty) = sym.type_info {
                    hover_text.push_str(&format!(
                        "```harn\n{keyword} {}: {}\n```\n",
                        sym.name,
                        format_type(ty)
                    ));
                } else {
                    let kind_str = match sym.kind {
                        HarnSymbolKind::Pipeline => "pipeline",
                        HarnSymbolKind::Function => "function",
                        HarnSymbolKind::Variable => "variable",
                        HarnSymbolKind::Parameter => "parameter",
                        HarnSymbolKind::Enum => "enum",
                        HarnSymbolKind::Struct => "struct",
                        HarnSymbolKind::Interface => "interface",
                    };
                    hover_text.push_str(&format!("**{kind_str}** `{}`", sym.name));
                }
            }

            // Signatures already show `-> type`; expand only shape types for
            // variables/params so complex shapes get a human-readable breakdown.
            // Tagged shape unions (union-of-shapes) also get an expanded view
            // so the variants are laid out vertically instead of collapsed
            // onto one line.
            if sym.signature.is_none() {
                if let Some(ref ty) = sym.type_info {
                    if matches!(ty, harn_parser::TypeExpr::Shape(_)) {
                        let expanded = format_shape_expanded(ty, 0);
                        if !expanded.is_empty() {
                            hover_text.push_str(&format!("\n{expanded}"));
                        }
                    } else if matches!(ty, harn_parser::TypeExpr::Union(_)) {
                        let expanded = format_union_shapes_expanded(ty);
                        if !expanded.is_empty() {
                            hover_text.push_str(&format!("\n{expanded}"));
                        }
                    }
                }
            }

            if let Some(ref doc) = sym.doc_comment {
                hover_text.push_str(&format!("\n---\n\n{doc}"));
            }

            let derived = sym.derived_example.as_deref();
            if let Some(meta) = sym.stdlib_metadata.as_ref().filter(|m| !m.is_empty()) {
                hover_text.push_str("\n\n---\n\n");
                hover_text.push_str(&meta.to_markdown_with_derived_example(derived));
            } else if let Some(derived) = derived {
                // No structured metadata (user scripts, undocumented fns):
                // still surface a usage example inferred from the signature.
                hover_text.push_str(&format!(
                    "\n\n---\n\n**Example** _(derived from signature)_\n\n```harn\n{derived}\n```"
                ));
            }

            if let Some(block) = format_flow_attributes_block(&sym.attributes) {
                hover_text.push_str(&block);
            }

            markup(hover_text)
        }
    }

    pub(super) async fn handle_signature_help(
        &self,
        params: SignatureHelpParams,
    ) -> Result<Option<SignatureHelp>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;

        let source = {
            let docs = self.documents.lock().unwrap();
            match docs.get(uri) {
                Some(s) if s.kind.is_harn() => s.source.clone(),
                _ => return Ok(None),
            }
        };

        let Some(prefix) = line_prefix_at_position(&source, position) else {
            return Ok(None);
        };

        let mut depth = 0i32;
        let mut comma_count = 0u32;
        let mut open_paren_pos = None;
        for (i, ch) in prefix.char_indices().rev() {
            match ch {
                ')' => depth += 1,
                '(' => {
                    if depth == 0 {
                        open_paren_pos = Some(i);
                        break;
                    }
                    depth -= 1;
                }
                ',' if depth == 0 => comma_count += 1,
                _ => {}
            }
        }

        let paren_pos = match open_paren_pos {
            Some(p) => p,
            None => return Ok(None),
        };

        let before = &prefix[..paren_pos];
        let name: String = before
            .chars()
            .rev()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect::<String>()
            .chars()
            .rev()
            .collect();

        if name.is_empty() {
            return Ok(None);
        }

        let sig_str = match builtin_signature(&name) {
            Some(sig) => sig,
            None => return Ok(None),
        };

        // Extract parameter fragment from `name(p1, p2, ...) -> ret`.
        let params_str = sig_str
            .split('(')
            .nth(1)
            .and_then(|s| s.split(')').next())
            .unwrap_or("");

        let params_list: Vec<ParameterInformation> = if params_str.is_empty() {
            vec![]
        } else {
            params_str
                .split(',')
                .map(|p| ParameterInformation {
                    label: ParameterLabel::Simple(p.trim().to_string()),
                    documentation: None,
                })
                .collect()
        };

        Ok(Some(SignatureHelp {
            signatures: vec![SignatureInformation {
                label: sig_str.to_string(),
                documentation: builtin_doc(&name).map(|d| {
                    Documentation::MarkupContent(MarkupContent {
                        kind: MarkupKind::Markdown,
                        value: d,
                    })
                }),
                parameters: Some(params_list.clone()),
                active_parameter: Some(if params_list.is_empty() {
                    0
                } else {
                    comma_count.min(params_list.len() as u32 - 1)
                }),
            }],
            active_signature: Some(0),
            active_parameter: Some(if params_list.is_empty() {
                0
            } else {
                comma_count.min(params_list.len() as u32 - 1)
            }),
        }))
    }

    pub(super) async fn handle_inlay_hint(
        &self,
        params: InlayHintParams,
    ) -> Result<Option<Vec<InlayHint>>> {
        let uri = params.text_document.uri;
        let docs = self.documents.lock().unwrap();
        let Some(state) = docs.get(&uri) else {
            return Ok(None);
        };

        let range = params.range;
        let hints: Vec<InlayHint> = state
            .inlay_hints
            .iter()
            .filter(|h| {
                let line = h.line.saturating_sub(1) as u32;
                line >= range.start.line && line <= range.end.line
            })
            .map(|h| InlayHint {
                position: Position::new(
                    h.line.saturating_sub(1) as u32,
                    h.column.saturating_sub(1) as u32,
                ),
                label: InlayHintLabel::String(h.label.clone()),
                kind: Some(InlayHintKind::TYPE),
                text_edits: None,
                tooltip: None,
                padding_left: None,
                padding_right: None,
                data: None,
            })
            .collect();

        Ok(if hints.is_empty() { None } else { Some(hints) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_prefix_uses_utf16_position_without_slicing_mid_char() {
        let source = SourceText::new("éé(log(");
        let prefix = line_prefix_at_position(&source, Position::new(0, 7)).unwrap();
        assert_eq!(prefix, source.as_str());
    }

    #[test]
    fn signature_prefix_rejects_out_of_range_line() {
        assert!(line_prefix_at_position(&SourceText::new("log("), Position::new(4, 0)).is_none());
    }
}
