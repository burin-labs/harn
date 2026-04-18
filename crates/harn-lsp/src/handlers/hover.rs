//! Hover, signature help, and inlay hints.

use harn_parser::format_type;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;

use crate::constants::{builtin_doc, keyword_doc, BUILTINS};
use crate::helpers::{lsp_position_to_offset, word_at_position};
use crate::symbols::{
    format_shape_expanded, format_union_shapes_expanded, HarnSymbolKind, SymbolInfo,
};
use crate::HarnLsp;

impl HarnLsp {
    pub(super) async fn handle_hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;

        let docs = self.documents.lock().unwrap();
        let state = match docs.get(uri) {
            Some(s) => s,
            None => return Ok(None),
        };
        let source = state.source.clone();
        let symbols = state.symbols.clone();
        drop(docs);

        let word = match word_at_position(&source, position) {
            Some(w) => w,
            None => return Ok(None),
        };

        if let Some(doc) = builtin_doc(&word) {
            return Ok(Some(Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: doc,
                }),
                range: None,
            }));
        }

        if let Some(doc) = keyword_doc(&word) {
            return Ok(Some(Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: doc,
                }),
                range: None,
            }));
        }

        // Check user-defined symbols — prefer the innermost scope that
        // contains the cursor position so that shadowed bindings resolve
        // to the closest definition.
        let cursor_offset = lsp_position_to_offset(&source, position);
        let mut best: Option<&SymbolInfo> = None;
        for sym in &symbols {
            if sym.name != word {
                continue;
            }
            // Impl-block methods are globally visible via dot syntax — skip scope check.
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
        if let Some(sym) = best {
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

            return Ok(Some(Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: hover_text,
                }),
                range: None,
            }));
        }

        Ok(None)
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
                Some(s) => s.source.clone(),
                None => return Ok(None),
            }
        };

        let lines: Vec<&str> = source.lines().collect();
        let line = match lines.get(position.line as usize) {
            Some(l) => *l,
            None => return Ok(None),
        };
        let col = position.character as usize;
        let prefix = if col <= line.len() {
            &line[..col]
        } else {
            line
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

        let sig_str = match BUILTINS.iter().find(|(n, _)| *n == name.as_str()) {
            Some((_, sig)) => *sig,
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
