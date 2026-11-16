use tree_sitter::Node;

use super::super::ShellWordExpansion;
use super::node_text;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum WordPosition {
    Argument,
    Redirect,
}

pub(super) fn shell_word(
    node: Node<'_>,
    source: &[u8],
    shell_id: Option<&str>,
    position: WordPosition,
) -> (Option<String>, ShellWordExpansion) {
    let Some(raw) = node_text(node, source) else {
        return (None, ShellWordExpansion::RuntimeValue);
    };
    let dynamic = requires_expansion(node, source, shell_id, position);
    match shell_words::split(raw) {
        Ok(words) if words.len() == 1 => (words.into_iter().next(), dynamic),
        _ => (None, dynamic),
    }
}

fn requires_expansion(
    root: Node<'_>,
    source: &[u8],
    shell_id: Option<&str>,
    position: WordPosition,
) -> ShellWordExpansion {
    let mut stack = vec![root];
    let mut brace_lists = Vec::new();
    let mut expansion = ShellWordExpansion::Literal;
    while let Some(node) = stack.pop() {
        match node.kind() {
            "raw_string" | "ansi_c_string" => continue,
            "arithmetic_expansion"
            | "command_substitution"
            | "expansion"
            | "process_substitution"
            | "simple_expansion" => return ShellWordExpansion::RuntimeValue,
            "brace_expression" if shell_id != Some("sh") => {
                return ShellWordExpansion::RuntimeValue;
            }
            "extglob_pattern" if shell_id != Some("sh") => expansion = ShellWordExpansion::Pathname,
            "word" => {
                let Some(raw) = node_text(node, source) else {
                    return ShellWordExpansion::RuntimeValue;
                };
                if node.start_byte() == root.start_byte() && raw.starts_with('~') {
                    return ShellWordExpansion::RuntimeValue;
                }
                let mut escaped = false;
                for character in raw.chars() {
                    if escaped {
                        escaped = false;
                    } else if character == '\\' {
                        escaped = true;
                    } else if matches!(character, '*' | '?' | '[')
                        && (position == WordPosition::Argument || shell_id != Some("sh"))
                    {
                        expansion = ShellWordExpansion::Pathname;
                    } else if shell_id != Some("sh") {
                        // Comma brace forms span unquoted tokens and quoted fragments.
                        match character {
                            '{' => brace_lists.push(false),
                            ',' => {
                                if let Some(list) = brace_lists.last_mut() {
                                    *list = true;
                                }
                            }
                            '}' if brace_lists.pop() == Some(true) => {
                                return ShellWordExpansion::RuntimeValue;
                            }
                            _ => {}
                        }
                    }
                }
            }
            _ => {}
        }
        let mut cursor = node.walk();
        let children = node.named_children(&mut cursor).collect::<Vec<_>>();
        stack.extend(children.into_iter().rev());
    }
    expansion
}
