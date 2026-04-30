//! Go-to-definition, find-references, and rename.

use std::collections::HashMap;

use harn_lexer::{Lexer, Span, TokenKind};
use harn_modules::DefKind;
use harn_parser::{Node, SNode};
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;

use crate::constants::BUILTINS;
use crate::helpers::{
    lsp_position_to_offset, offset_to_position, span_to_full_range, word_at_position,
};
use crate::references::find_references;
use crate::symbols::HarnSymbolKind;
use crate::HarnLsp;

impl HarnLsp {
    pub(super) async fn handle_goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;

        let docs = self.documents.lock().unwrap();
        let state = match docs.get(uri) {
            Some(s) => s,
            None => return Ok(None),
        };
        let source = state.source.clone();
        let symbols = state.symbols.clone();
        let ast = state.cached_ast.clone();
        drop(docs);

        // Inside a `render(...)` / `render_prompt(...)` string literal,
        // jump straight to the referenced `.harn.prompt` file. Honors
        // package-root forms (`@/...`, `@<alias>/...`) via the same
        // resolver the runtime and preflight checks use (#742).
        if let Some(program) = &ast {
            if let Some(loc) = resolve_prompt_asset_definition(uri, &source, position, program) {
                return Ok(Some(GotoDefinitionResponse::Scalar(loc)));
            }
        }

        let word = match word_at_position(&source, position) {
            Some(w) => w,
            None => return Ok(None),
        };

        for sym in &symbols {
            if sym.name == word
                && matches!(
                    sym.kind,
                    HarnSymbolKind::Pipeline
                        | HarnSymbolKind::Function
                        | HarnSymbolKind::Variable
                        | HarnSymbolKind::Parameter
                        | HarnSymbolKind::Enum
                        | HarnSymbolKind::Struct
                        | HarnSymbolKind::Interface
                )
            {
                let range = span_to_full_range(&sym.def_span, &source);
                return Ok(Some(GotoDefinitionResponse::Scalar(Location {
                    uri: uri.clone(),
                    range,
                })));
            }
        }

        // Cross-file: the module graph transitively follows imports from
        // this file, so there's no need to pre-walk the AST here.
        if let Some(loc) = resolve_cross_file_definition(uri, &word) {
            return Ok(Some(GotoDefinitionResponse::Scalar(loc)));
        }

        Ok(None)
    }

    pub(super) async fn handle_references(
        &self,
        params: ReferenceParams,
    ) -> Result<Option<Vec<Location>>> {
        let uri = &params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;

        let docs = self.documents.lock().unwrap();
        let state = match docs.get(uri) {
            Some(s) => s,
            None => return Ok(None),
        };
        let source = state.source.clone();
        let ast = state.cached_ast.clone();
        drop(docs);

        let word = match word_at_position(&source, position) {
            Some(w) => w,
            None => return Ok(None),
        };

        let program = match ast {
            Some(p) => p,
            None => return Ok(None),
        };

        let ref_spans = find_references(&program, &word);
        if ref_spans.is_empty() {
            return Ok(None);
        }

        let locations: Vec<Location> = ref_spans
            .iter()
            .map(|span| Location {
                uri: uri.clone(),
                range: span_to_full_range(span, &source),
            })
            .collect();

        Ok(Some(locations))
    }

    pub(super) async fn handle_rename(
        &self,
        params: RenameParams,
    ) -> Result<Option<WorkspaceEdit>> {
        let uri = &params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        let new_name = &params.new_name;

        let docs = self.documents.lock().unwrap();
        let state = match docs.get(uri) {
            Some(s) => s,
            None => return Ok(None),
        };
        let source = state.source.clone();
        let ast = state.cached_ast.clone();
        let symbols = state.symbols.clone();
        drop(docs);

        let old_name = match word_at_position(&source, position) {
            Some(w) => w,
            None => return Ok(None),
        };

        // Builtins must not be renamed.
        if BUILTINS.iter().any(|(n, _)| *n == old_name) {
            return Ok(None);
        }

        let symbol_exists = symbols.iter().any(|s| s.name == old_name);
        if !symbol_exists {
            return Ok(None);
        }

        let program = match ast {
            Some(p) => p,
            None => return Ok(None),
        };
        let ref_spans = find_references(&program, &old_name);
        if ref_spans.is_empty() {
            return Ok(None);
        }

        // AST reference spans cover whole declarations, so rescan the lexer
        // tokens within each span to pin down the exact identifier position.
        let mut edits = Vec::new();
        let mut seen_offsets = std::collections::HashSet::new();

        let mut lexer = Lexer::new(&source);
        if let Ok(tokens) = lexer.tokenize() {
            for token in &tokens {
                if let TokenKind::Identifier(ref name) = token.kind {
                    if name == &old_name && !seen_offsets.contains(&token.span.start) {
                        let in_ref = ref_spans
                            .iter()
                            .any(|rs| token.span.start >= rs.start && token.span.end <= rs.end);
                        if in_ref {
                            seen_offsets.insert(token.span.start);
                            let start = offset_to_position(&source, token.span.start);
                            let end = offset_to_position(&source, token.span.end);
                            edits.push(TextEdit {
                                range: Range { start, end },
                                new_text: new_name.clone(),
                            });
                        }
                    }
                }
            }
        }

        if edits.is_empty() {
            return Ok(None);
        }

        // Sort bottom-up so applying edits doesn't shift later offsets.
        edits.sort_by(|a, b| {
            b.range
                .start
                .line
                .cmp(&a.range.start.line)
                .then(b.range.start.character.cmp(&a.range.start.character))
        });

        let mut changes = HashMap::new();
        changes.insert(uri.clone(), edits);

        Ok(Some(WorkspaceEdit {
            changes: Some(changes),
            ..Default::default()
        }))
    }
}

/// When the cursor sits inside a string literal that's the first
/// argument to a literal `render(...)` or `render_prompt(...)` call,
/// resolve the path (source-relative or `@/...` / `@<alias>/...`) and
/// return a `Location` pointing at the prompt file's first byte. Returns
/// `None` for any other context, so callers can fall through to symbol
/// resolution.
fn resolve_prompt_asset_definition(
    uri: &Url,
    source: &str,
    position: Position,
    program: &[SNode],
) -> Option<Location> {
    let offset = lsp_position_to_offset(source, position);
    let (template_path, _) = find_render_string_at_offset(program, offset)?;
    let current_path = uri.to_file_path().ok()?;
    let resolved = if let Some(asset_ref) = harn_modules::asset_paths::parse(&template_path) {
        let anchor = current_path.parent().unwrap_or(std::path::Path::new("."));
        harn_modules::asset_paths::resolve(&asset_ref, anchor).ok()?
    } else if std::path::Path::new(&template_path).is_absolute() {
        std::path::PathBuf::from(&template_path)
    } else {
        current_path
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .join(&template_path)
    };
    if !resolved.exists() {
        return None;
    }
    let target_uri = Url::from_file_path(&resolved).ok()?;
    Some(Location {
        uri: target_uri,
        range: Range {
            start: Position::new(0, 0),
            end: Position::new(0, 0),
        },
    })
}

fn find_render_string_at_offset(program: &[SNode], offset: usize) -> Option<(String, Span)> {
    for node in program {
        if let Some(hit) = find_render_string_in_node(node, offset) {
            return Some(hit);
        }
    }
    None
}

fn find_render_string_in_node(node: &SNode, offset: usize) -> Option<(String, Span)> {
    if let Node::FunctionCall { name, args } = &node.node {
        if (name == "render" || name == "render_prompt") && !args.is_empty() {
            if let Node::StringLiteral(value) = &args[0].node {
                let span = args[0].span;
                if span_contains_offset(&span, offset) {
                    return Some((value.clone(), span));
                }
            }
        }
    }
    for child in node_children(node) {
        if let Some(hit) = find_render_string_in_node(child, offset) {
            return Some(hit);
        }
    }
    None
}

fn span_contains_offset(span: &Span, offset: usize) -> bool {
    offset >= span.start && offset <= span.end
}

fn node_children(node: &SNode) -> Vec<&SNode> {
    match &node.node {
        Node::Pipeline { body, .. }
        | Node::OverrideDecl { body, .. }
        | Node::SpawnExpr { body }
        | Node::Block(body)
        | Node::Closure { body, .. }
        | Node::TryExpr { body }
        | Node::MutexBlock { body }
        | Node::DeferStmt { body } => body.iter().collect(),
        Node::FnDecl { body, .. } | Node::ToolDecl { body, .. } => body.iter().collect(),
        Node::IfElse {
            condition,
            then_body,
            else_body,
        } => {
            let mut out = vec![condition.as_ref()];
            out.extend(then_body.iter());
            if let Some(eb) = else_body {
                out.extend(eb.iter());
            }
            out
        }
        Node::ForIn { iterable, body, .. } => {
            let mut out = vec![iterable.as_ref()];
            out.extend(body.iter());
            out
        }
        Node::WhileLoop { condition, body }
        | Node::GuardStmt {
            condition,
            else_body: body,
        } => {
            let mut out = vec![condition.as_ref()];
            out.extend(body.iter());
            out
        }
        Node::CostRoute { options, body } => {
            let mut out = options.iter().map(|(_, value)| value).collect::<Vec<_>>();
            out.extend(body.iter());
            out
        }
        Node::TryCatch {
            body,
            catch_body,
            finally_body,
            ..
        } => {
            let mut out = body.iter().collect::<Vec<_>>();
            out.extend(catch_body.iter());
            if let Some(fb) = finally_body {
                out.extend(fb.iter());
            }
            out
        }
        Node::FunctionCall { args, .. } => args.iter().collect(),
        Node::MethodCall { object, args, .. } | Node::OptionalMethodCall { object, args, .. } => {
            let mut out = vec![object.as_ref()];
            out.extend(args.iter());
            out
        }
        Node::PropertyAccess { object, .. }
        | Node::OptionalPropertyAccess { object, .. }
        | Node::UnaryOp {
            operand: object, ..
        }
        | Node::ThrowStmt { value: object }
        | Node::Spread(object)
        | Node::TryOperator { operand: object }
        | Node::TryStar { operand: object } => vec![object.as_ref()],
        Node::SubscriptAccess { object, index }
        | Node::OptionalSubscriptAccess { object, index } => {
            vec![object.as_ref(), index.as_ref()]
        }
        Node::BinaryOp { left, right, .. }
        | Node::Assignment {
            target: left,
            value: right,
            ..
        } => vec![left.as_ref(), right.as_ref()],
        Node::Ternary {
            condition,
            true_expr,
            false_expr,
        } => vec![condition.as_ref(), true_expr.as_ref(), false_expr.as_ref()],
        Node::DictLiteral(fields) | Node::StructConstruct { fields, .. } => {
            let mut out = Vec::new();
            for f in fields {
                out.push(&f.key);
                out.push(&f.value);
            }
            out
        }
        Node::EnumConstruct { args, .. } | Node::ListLiteral(args) => args.iter().collect(),
        Node::LetBinding { value, .. } | Node::VarBinding { value, .. } => vec![value.as_ref()],
        Node::ReturnStmt { value } | Node::YieldExpr { value } => {
            value.iter().map(|v| v.as_ref()).collect()
        }
        Node::EmitExpr { value } => vec![value.as_ref()],
        Node::AttributedDecl { inner, .. } => vec![inner.as_ref()],
        _ => Vec::new(),
    }
}

/// Resolve the symbol through the current document's imported modules using
/// `harn-modules`, and return its definition location when available.
///
/// `harn_modules::build` recursively follows import paths, so seeding it
/// with the current file is enough to discover every module reachable via
/// imports.
fn resolve_cross_file_definition(uri: &Url, word: &str) -> Option<Location> {
    let current_path = uri.to_file_path().ok()?;
    let module_graph = harn_modules::build(std::slice::from_ref(&current_path));
    let def = module_graph.definition_of(&current_path, word)?;
    if !matches!(
        def.kind,
        DefKind::Pipeline
            | DefKind::Function
            | DefKind::Variable
            | DefKind::Parameter
            | DefKind::Enum
            | DefKind::Struct
            | DefKind::Interface
    ) {
        return None;
    }
    let imported_source = std::fs::read_to_string(&def.file).ok()?;
    let imported_uri = Url::from_file_path(&def.file).ok()?;
    Some(Location {
        uri: imported_uri,
        range: span_to_full_range(&def.span, &imported_source),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use harn_parser::parse_source;

    #[test]
    fn finds_render_prompt_string_under_cursor() {
        let source = r#"
pipeline test() {
  let x = render_prompt("@/prompts/foo.harn.prompt", {})
  println(x)
}
"#;
        let program = parse_source(source).expect("parse");
        // Cursor inside the string literal — anywhere in the quoted span.
        let cursor = source.find("@/prompts").unwrap() + 3;
        let (path, _) =
            find_render_string_at_offset(&program, cursor).expect("should locate the asset string");
        assert_eq!(path, "@/prompts/foo.harn.prompt");
    }

    #[test]
    fn ignores_other_function_calls() {
        let source = r#"
pipeline test() {
  let x = println("@/not-a-prompt")
}
"#;
        let program = parse_source(source).expect("parse");
        let cursor = source.find("@/not-a-prompt").unwrap() + 3;
        assert!(find_render_string_at_offset(&program, cursor).is_none());
    }

    #[test]
    fn finds_render_string_outside_string_returns_none() {
        let source = r#"
pipeline test() {
  let x = render_prompt("@/prompts/foo.harn.prompt", {})
}
"#;
        let program = parse_source(source).expect("parse");
        // Cursor on `render_prompt` identifier — not inside the string.
        let cursor = source.find("render_prompt").unwrap() + 2;
        assert!(find_render_string_at_offset(&program, cursor).is_none());
    }
}
