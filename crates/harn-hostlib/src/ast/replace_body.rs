use super::function_body::{extract_body, BodyLookup};
use super::language::Language;
use super::parse::parse_source;

/// Construct bytes only. The symbol mutation owner checks uniqueness, parser
/// fitness and candidate syntax before returning this plan to the host writer.
pub(super) fn replacement(
    tree: &tree_sitter::Tree,
    source: &str,
    language: Language,
    symbol_name: &str,
    new_text: &str,
) -> Option<String> {
    let (container, name) = symbol_name
        .split_once('.')
        .map_or((None, symbol_name), |(container, name)| {
            (Some(container), name)
        });
    let BodyLookup::Isolated(body) = extract_body(tree, source, language, name, container) else {
        return None;
    };
    let range = body.byte_range?;
    let mut incoming = new_text.trim_matches(['\r', '\n']).to_owned();
    if incoming.trim().is_empty() {
        return None;
    }
    incoming = unwrap_function_body(&incoming, language, name).unwrap_or(incoming);
    if incoming.trim().is_empty() {
        return None;
    }
    if contains_multiline_token(&incoming, language)? {
        return None;
    }
    let before = source.get(..range.start)?;
    let line_start = before.rfind('\n').map_or(0, |pos| pos + 1);
    let prefix = source.get(line_start..range.start)?;
    let base: String = prefix.chars().take_while(|ch| ch.is_whitespace()).collect();
    let braced = body.body_text.starts_with('{') && body.body_text.ends_with('}');
    let indent = if braced {
        body.body_text
            .lines()
            .skip(1)
            .find_map(|line| {
                (!line.trim().is_empty() && line.trim() != "}")
                    .then(|| {
                        line.chars()
                            .take_while(|ch| *ch == ' ' || *ch == '\t')
                            .collect::<String>()
                    })
                    .filter(|indent| indent.len() > base.len())
            })
            .unwrap_or_else(|| format!("{base}    "))
    } else if prefix.chars().all(char::is_whitespace) {
        prefix.to_owned()
    } else {
        return None;
    };
    let dedented = dedent(&incoming);
    let replacement = if braced {
        let inner = dedented
            .lines()
            .map(|line| format!("{indent}{line}"))
            .collect::<Vec<_>>()
            .join("\n");
        format!("{{\n{inner}\n{base}}}")
    } else {
        dedented
            .lines()
            .enumerate()
            .map(|(index, line)| {
                if index == 0 {
                    line.to_owned()
                } else {
                    format!("{indent}{line}")
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    Some(format!(
        "{}{}{}",
        before,
        replacement,
        source.get(range.end..)?
    ))
}

// Reindentation would alter newline-bearing token bytes, including multiline
// literals. Keep those edits on the exact-text path instead.
fn contains_multiline_token(source: &str, language: Language) -> Option<bool> {
    let tree = parse_source(source, language).ok()?;
    let mut pending = vec![tree.root_node()];
    while let Some(node) = pending.pop() {
        if node.child_count() == 0 {
            if node.start_position().row != node.end_position().row {
                return Some(true);
            }
        } else {
            let mut cursor = node.walk();
            pending.extend(node.children(&mut cursor));
        }
    }
    Some(false)
}

// Unwrap only a precise declaration that covers the complete input.
fn unwrap_function_body(source: &str, language: Language, name: &str) -> Option<String> {
    let tree = parse_source(source, language).ok()?;
    if tree.root_node().has_error() || tree.root_node().named_child_count() != 1 {
        return None;
    }
    let BodyLookup::Isolated(wrapper) = extract_body(&tree, source, language, name, None) else {
        return None;
    };
    if wrapper.byte_range.is_none()
        || wrapper.declaration.start_line != 1
        || wrapper.declaration.end_line as usize != source.lines().count()
    {
        return None;
    }
    let inner = wrapper.body_text;
    Some(if inner.starts_with('{') && inner.ends_with('}') {
        inner
            .get(1..inner.len() - 1)?
            .trim_matches(['\r', '\n'])
            .to_owned()
    } else {
        inner
    })
}

fn dedent(text: &str) -> String {
    let lines: Vec<_> = text.trim_matches(['\r', '\n']).lines().collect();
    let depth = lines
        .iter()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            line.bytes()
                .take_while(|ch| *ch == b' ' || *ch == b'\t')
                .count()
        })
        .min()
        .unwrap_or(0);
    lines
        .iter()
        .map(|line| {
            if line.trim().is_empty() {
                ""
            } else {
                line.get(depth..).unwrap_or(line)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}
