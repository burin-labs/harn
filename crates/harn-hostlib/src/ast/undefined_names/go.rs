//! Go profile for \`ast.undefined_names\`.

use super::*;

pub(super) static BUILTINS: std::sync::LazyLock<HashSet<&'static str>> =
    std::sync::LazyLock::new(|| {
        [
            "true",
            "false",
            "nil",
            "iota",
            "append",
            "cap",
            "close",
            "complex",
            "copy",
            "delete",
            "imag",
            "len",
            "make",
            "new",
            "panic",
            "print",
            "println",
            "real",
            "recover",
            "any",
            "bool",
            "byte",
            "comparable",
            "complex64",
            "complex128",
            "error",
            "float32",
            "float64",
            "int",
            "int8",
            "int16",
            "int32",
            "int64",
            "rune",
            "string",
            "uint",
            "uint8",
            "uint16",
            "uint32",
            "uint64",
            "uintptr",
            "_",
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
        "import_spec" => {
            if let Some(name) = node.child_by_field_name("name") {
                defined.insert(node_text(name, source).to_string());
            } else if let Some(path) = node.child_by_field_name("path") {
                let raw = node_text(path, source).trim_matches('"');
                if let Some(last) = raw.rsplit('/').next() {
                    defined.insert(last.to_string());
                }
            }
        }
        "function_declaration" | "method_declaration" => {
            if let Some(name) = node.child_by_field_name("name") {
                defined.insert(node_text(name, source).to_string());
            }
            if let Some(receiver) = node.child_by_field_name("receiver") {
                collect_go_field_idents(receiver, source, defined);
            }
            if let Some(params) = node.child_by_field_name("parameters") {
                collect_go_field_idents(params, source, defined);
            }
            if let Some(result) = node.child_by_field_name("result") {
                visit(result, source, defined, refs);
            }
            if let Some(body) = node.child_by_field_name("body") {
                visit(body, source, defined, refs);
            }
        }
        "type_declaration" => {
            let mut vis = |child: Node<'_>| {
                if child.kind() == "type_spec" {
                    if let Some(name) = child.child_by_field_name("name") {
                        defined.insert(node_text(name, source).to_string());
                    }
                }
            };
            walk(node, &mut vis);
            let mut cursor = node.walk();
            for spec in node.named_children(&mut cursor) {
                if let Some(t) = spec.child_by_field_name("type") {
                    visit(t, source, defined, refs);
                }
            }
        }
        "short_var_declaration" | "var_spec" | "const_spec" => {
            if let Some(left) = node.child_by_field_name("left") {
                let mut cursor = left.walk();
                for child in left.named_children(&mut cursor) {
                    if child.kind() == "identifier" {
                        defined.insert(node_text(child, source).to_string());
                    }
                }
            } else {
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    if child.kind() == "identifier" {
                        defined.insert(node_text(child, source).to_string());
                    } else {
                        break;
                    }
                }
            }
            if let Some(right) = node.child_by_field_name("right") {
                visit(right, source, defined, refs);
            }
            if let Some(t) = node.child_by_field_name("type") {
                visit(t, source, defined, refs);
            }
        }
        "range_clause" => {
            if let Some(left) = node.child_by_field_name("left") {
                let mut cursor = left.walk();
                for child in left.named_children(&mut cursor) {
                    if child.kind() == "identifier" {
                        defined.insert(node_text(child, source).to_string());
                    }
                }
            }
            if let Some(right) = node.child_by_field_name("right") {
                visit(right, source, defined, refs);
            }
        }
        "selector_expression" => {
            if let Some(operand) = node.child_by_field_name("operand") {
                visit(operand, source, defined, refs);
            }
        }
        "field_identifier" | "type_identifier" => {}
        "keyed_element" => {
            let count = node.named_child_count();
            for i in 1..count {
                if let Some(child) = node.named_child(i as u32) {
                    visit(child, source, defined, refs);
                }
            }
        }
        "identifier" => {
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

fn collect_go_field_idents(node: Node<'_>, source: &str, defined: &mut HashSet<String>) {
    let mut vis = |child: Node<'_>| {
        if child.kind() == "parameter_declaration" {
            let mut cursor = child.walk();
            for c in child.named_children(&mut cursor) {
                if c.kind() == "identifier" {
                    defined.insert(node_text(c, source).to_string());
                }
            }
        }
    };
    walk(node, &mut vis);
}
