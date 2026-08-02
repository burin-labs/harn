use super::*;

use harn_lexer::{Token, TokenKind};
use harn_parser::{peel_attributes, BindingPattern, Node, SNode};

struct PublicDeclaration<'a> {
    kind: &'static str,
    name: &'a str,
    inner: &'a SNode,
    has_body: bool,
}

/// Extract public declarations from the parser's top-level declaration spans.
///
/// The AST owns declaration extent. In particular, a source line is not a
/// semantic boundary: callable parameters and type expressions may wrap over
/// any number of lines. Body-bearing declarations keep only their header,
/// while aliases and constants retain their complete expression.
pub(crate) fn extract_api_symbols(source: &str) -> Vec<PackageApiSymbol> {
    let mut lexer = harn_lexer::Lexer::new(source);
    let Ok(tokens) = lexer.tokenize_with_comments() else {
        return Vec::new();
    };
    let parser_tokens = tokens
        .iter()
        .filter(|token| !is_comment(&token.kind))
        .cloned()
        .collect::<Vec<_>>();
    let mut parser = harn_parser::Parser::new(parser_tokens.clone());
    let Ok(program) = parser.parse() else {
        return Vec::new();
    };

    program
        .iter()
        .filter_map(|node| {
            let declaration = public_declaration(node)?;
            let pub_token = parser_tokens.iter().rev().find(|token| {
                matches!(&token.kind, TokenKind::Pub)
                    && token.span.end <= declaration.inner.span.start
            })?;
            let signature_end = if declaration.has_body {
                outer_body_start(&parser_tokens, declaration.inner)?
            } else {
                declaration.inner.span.end
            };
            let raw_signature = source
                .get(pub_token.span.start..signature_end)?
                .trim()
                .replace("\r\n", "\n");
            let signature_source = if declaration.has_body {
                format!("{raw_signature} {{}}\n")
            } else {
                format!("{raw_signature}\n")
            };
            let signature = harn_fmt::format_source(&signature_source)
                .unwrap_or(signature_source)
                .trim()
                .to_string();
            let pub_index = tokens
                .iter()
                .position(|token| token.span.start == pub_token.span.start)?;
            let docs_index = if matches!(&node.node, Node::AttributedDecl { .. }) {
                tokens
                    .iter()
                    .position(|token| token.span.start == node.span.start)
                    .unwrap_or(pub_index)
            } else {
                pub_index
            };

            Some(PackageApiSymbol {
                kind: declaration.kind.to_string(),
                name: declaration.name.to_string(),
                signature,
                docs: docs_before(&tokens, docs_index),
            })
        })
        .collect()
}

/// Extract the complete public surface of `path`, including symbols forwarded
/// through selective, wildcard, or transitive `pub import` declarations.
///
/// Local declarations retain source order for backward-compatible docs. A
/// facade's forwarded declarations follow in deterministic name order, using
/// the original declaration's signature and HarnDoc rather than synthesizing
/// pass-through metadata at the import site.
pub(crate) fn extract_api_symbols_for_module(
    path: &Path,
    graph: &harn_modules::ModuleGraph,
) -> Vec<PackageApiSymbol> {
    let source = harn_modules::read_module_source(path).unwrap_or_default();
    let mut symbols = extract_api_symbols(&source);
    let local_names = symbols
        .iter()
        .map(|symbol| symbol.name.clone())
        .collect::<HashSet<_>>();
    let mut origin_symbols = HashMap::<PathBuf, Vec<PackageApiSymbol>>::new();
    for name in graph.exports_for_module(path) {
        if local_names.contains(&name) {
            continue;
        }
        let Some(definition) = graph.export_definition_of(path, &name) else {
            continue;
        };
        let candidates = origin_symbols
            .entry(definition.file)
            .or_insert_with_key(|file| {
                harn_modules::read_module_source(file)
                    .map(|source| extract_api_symbols(&source))
                    .unwrap_or_default()
            });
        if let Some(symbol) = candidates.iter().find(|symbol| symbol.name == name) {
            symbols.push(symbol.clone());
        }
    }
    symbols
}

fn public_declaration<'a>(node: &'a SNode) -> Option<PublicDeclaration<'a>> {
    let (_, inner) = peel_attributes(node);
    let (kind, name, is_pub, has_body) = match &inner.node {
        Node::FnDecl { name, is_pub, .. } => ("fn", name.as_str(), *is_pub, true),
        Node::Pipeline { name, is_pub, .. } => ("pipeline", name.as_str(), *is_pub, true),
        Node::ToolDecl { name, is_pub, .. } => ("tool", name.as_str(), *is_pub, true),
        Node::SkillDecl { name, is_pub, .. } => ("skill", name.as_str(), *is_pub, true),
        Node::StructDecl { name, is_pub, .. } => ("struct", name.as_str(), *is_pub, true),
        Node::EnumDecl { name, is_pub, .. } => ("enum", name.as_str(), *is_pub, true),
        Node::TypeDecl { name, is_pub, .. } => ("type", name.as_str(), *is_pub, false),
        Node::ConstBinding {
            pattern: BindingPattern::Identifier(name),
            is_pub,
            ..
        } => ("const", name.as_str(), *is_pub, false),
        _ => return None,
    };
    is_pub.then_some(PublicDeclaration {
        kind,
        name,
        inner,
        has_body,
    })
}

/// Find the opening brace whose matching closing brace terminates the parsed
/// declaration. Scanning backward makes return-shape braces and nested body
/// braces irrelevant: only the brace paired with the declaration's final token
/// can be its body boundary.
fn outer_body_start(tokens: &[Token], declaration: &SNode) -> Option<usize> {
    let mut depth = 0usize;
    for token in tokens.iter().rev().filter(|token| {
        token.span.start >= declaration.span.start && token.span.end <= declaration.span.end
    }) {
        match &token.kind {
            TokenKind::RBrace => depth += 1,
            TokenKind::LBrace => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(token.span.start);
                }
            }
            _ => {}
        }
    }
    None
}

fn is_comment(kind: &TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::LineComment { .. } | TokenKind::BlockComment { .. }
    )
}

fn docs_before(tokens: &[Token], declaration_index: usize) -> Option<String> {
    let mut docs = Vec::new();
    for token in tokens[..declaration_index].iter().rev() {
        match &token.kind {
            TokenKind::Newline => continue,
            TokenKind::LineComment { text, is_doc } if *is_doc => {
                let text = text.trim();
                if !text.is_empty() {
                    docs.push(text.to_string());
                }
            }
            TokenKind::BlockComment { text, is_doc } if *is_doc => {
                let text = text
                    .lines()
                    .map(|line| line.trim().strip_prefix('*').unwrap_or(line.trim()).trim())
                    .filter(|line| !line.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n");
                if !text.is_empty() {
                    docs.push(text);
                }
            }
            _ => break,
        }
    }
    (!docs.is_empty()).then(|| {
        docs.reverse();
        docs.join("\n")
    })
}
