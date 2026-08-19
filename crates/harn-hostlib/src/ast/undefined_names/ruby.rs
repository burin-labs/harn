//! Ruby profile for \`ast.undefined_names\`.

use super::*;

pub(super) static BUILTINS: std::sync::LazyLock<HashSet<&'static str>> =
    std::sync::LazyLock::new(|| {
        [
            "self",
            "super",
            "nil",
            "true",
            "false",
            "__method__",
            "__callee__",
            "Array",
            "Hash",
            "String",
            "Integer",
            "Float",
            "Symbol",
            "Range",
            "Regexp",
            "Proc",
            "Lambda",
            "Module",
            "Class",
            "Object",
            "Kernel",
            "Comparable",
            "Enumerable",
            "Exception",
            "StandardError",
            "RuntimeError",
            "ArgumentError",
            "TypeError",
            "NameError",
            "NoMethodError",
            "IOError",
            "p",
            "pp",
            "print",
            "puts",
            "gets",
            "require",
            "require_relative",
            "load",
            "raise",
            "throw",
            "catch",
            "lambda",
            "proc",
            "yield",
            "attr_reader",
            "attr_writer",
            "attr_accessor",
            "initialize",
            "include",
            "extend",
            "prepend",
            "private",
            "public",
            "protected",
        ]
        .into_iter()
        .collect()
    });

pub(super) fn collect(
    root: Node<'_>,
    source: &str,
    defined: &mut HashSet<String>,
    refs: &mut Vec<UndefinedName>,
) {
    visit(root, source, defined, refs);
}

fn visit(
    node: Node<'_>,
    source: &str,
    defined: &mut HashSet<String>,
    refs: &mut Vec<UndefinedName>,
) {
    match node.kind() {
        "method" | "singleton_method" => {
            if let Some(name) = node.child_by_field_name("name") {
                defined.insert(node_text(name, source).to_string());
            }
            if let Some(params) = node.child_by_field_name("parameters") {
                let mut vis = |child: Node<'_>| {
                    if child.kind() == "identifier" || child.kind() == "simple_symbol" {
                        defined.insert(node_text(child, source).to_string());
                    }
                };
                walk(params, &mut vis);
            }
            if let Some(body) = node.child_by_field_name("body") {
                visit(body, source, defined, refs);
            }
        }
        "class" | "module" => {
            if let Some(name) = node.child_by_field_name("name") {
                defined.insert(node_text(name, source).to_string());
            }
            if let Some(body) = node.child_by_field_name("body") {
                visit(body, source, defined, refs);
            }
        }
        "assignment" | "operator_assignment" => {
            if let Some(right) = node.child_by_field_name("right") {
                visit(right, source, defined, refs);
            }
            if let Some(left) = node.child_by_field_name("left") {
                if left.kind() == "identifier" || left.kind() == "constant" {
                    defined.insert(node_text(left, source).to_string());
                } else {
                    visit(left, source, defined, refs);
                }
            }
        }
        "block_parameters" | "method_parameters" => {
            let mut vis = |child: Node<'_>| {
                if child.kind() == "identifier" {
                    defined.insert(node_text(child, source).to_string());
                }
            };
            walk(node, &mut vis);
        }
        "call" => {
            if let Some(receiver) = node.child_by_field_name("receiver") {
                visit(receiver, source, defined, refs);
            } else if let Some(method) = node.child_by_field_name("method") {
                visit(method, source, defined, refs);
            }
            if let Some(args) = node.child_by_field_name("arguments") {
                visit(args, source, defined, refs);
            }
            if let Some(block) = node.child_by_field_name("block") {
                visit(block, source, defined, refs);
            }
        }
        "hash_key_symbol" | "simple_symbol" => {}
        "pair" => {
            if let Some(value) = node.child_by_field_name("value") {
                visit(value, source, defined, refs);
            }
        }
        "identifier" | "constant" => {
            add_reference(refs, node, source, "identifier");
        }
        _ => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                visit(child, source, defined, refs);
            }
        }
    }
}
