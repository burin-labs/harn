//! JavaScript and TypeScript profile for \`ast.undefined_names\`.

use super::*;

pub(super) static JS_BUILTINS: std::sync::LazyLock<HashSet<&'static str>> =
    std::sync::LazyLock::new(base_builtins);
pub(super) static TS_BUILTINS: std::sync::LazyLock<HashSet<&'static str>> =
    std::sync::LazyLock::new(|| {
        let mut set = base_builtins();
        // TS-only builtin type names so references don't fire on every
        // `string` / `number`.
        for extra in [
            "string",
            "number",
            "boolean",
            "any",
            "unknown",
            "never",
            "void",
            "object",
            "bigint",
            "symbol",
            "Record",
            "Partial",
            "Required",
            "Readonly",
            "Pick",
            "Omit",
            "Exclude",
            "Extract",
            "NonNullable",
            "Parameters",
            "ReturnType",
            "InstanceType",
            "ThisParameterType",
            "OmitThisParameter",
            "ThisType",
            "Awaited",
            "Array",
        ] {
            set.insert(extra);
        }
        set
    });

fn base_builtins() -> HashSet<&'static str> {
    [
        "undefined",
        "null",
        "true",
        "false",
        "this",
        "arguments",
        "globalThis",
        "console",
        "process",
        "window",
        "document",
        "navigator",
        "setTimeout",
        "clearTimeout",
        "setInterval",
        "clearInterval",
        "setImmediate",
        "queueMicrotask",
        "requestAnimationFrame",
        "Promise",
        "Error",
        "TypeError",
        "RangeError",
        "SyntaxError",
        "ReferenceError",
        "URIError",
        "EvalError",
        "Array",
        "Object",
        "String",
        "Number",
        "Boolean",
        "Symbol",
        "BigInt",
        "RegExp",
        "Date",
        "Math",
        "JSON",
        "Map",
        "Set",
        "WeakMap",
        "WeakSet",
        "Int8Array",
        "Int16Array",
        "Int32Array",
        "Uint8Array",
        "Uint16Array",
        "Uint32Array",
        "Uint8ClampedArray",
        "Float32Array",
        "Float64Array",
        "ArrayBuffer",
        "DataView",
        "SharedArrayBuffer",
        "Atomics",
        "parseInt",
        "parseFloat",
        "isNaN",
        "isFinite",
        "encodeURI",
        "decodeURI",
        "encodeURIComponent",
        "decodeURIComponent",
        "escape",
        "unescape",
        "NaN",
        "Infinity",
        "require",
        "module",
        "exports",
        "__dirname",
        "__filename",
        "Buffer",
        "React",
        "JSX",
        "async",
        "await",
    ]
    .into_iter()
    .collect()
}

pub(super) fn collect(
    root: Node<'_>,
    source: &str,
    defined: &mut HashSet<String>,
    refs: &mut Vec<UndefinedName>,
    typescript: bool,
) {
    visit(root, source, defined, refs, typescript);
}

fn bind_pattern(
    node: Node<'_>,
    source: &str,
    defined: &mut HashSet<String>,
    refs: &mut Vec<UndefinedName>,
    typescript: bool,
) {
    match node.kind() {
        "identifier" | "shorthand_property_identifier_pattern" => {
            defined.insert(node_text(node, source).to_string());
        }
        "array_pattern" | "object_pattern" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                bind_pattern(child, source, defined, refs, typescript);
            }
        }
        "assignment_pattern" => {
            if let Some(left) = node.child_by_field_name("left") {
                bind_pattern(left, source, defined, refs, typescript);
            }
            if let Some(right) = node.child_by_field_name("right") {
                visit(right, source, defined, refs, typescript);
            }
        }
        "rest_pattern" => {
            if let Some(ident) = first_identifier(node) {
                defined.insert(node_text(ident, source).to_string());
            }
        }
        "pair_pattern" => {
            if let Some(value) = node.child_by_field_name("value") {
                bind_pattern(value, source, defined, refs, typescript);
            }
        }
        _ => {
            if let Some(ident) = first_identifier(node) {
                defined.insert(node_text(ident, source).to_string());
            }
        }
    }
}

fn visit(
    node: Node<'_>,
    source: &str,
    defined: &mut HashSet<String>,
    refs: &mut Vec<UndefinedName>,
    typescript: bool,
) {
    match node.kind() {
        "import_statement" => {
            let mut vis = |child: Node<'_>| match child.kind() {
                "identifier" if field_name(child) != Some("source") => {
                    defined.insert(node_text(child, source).to_string());
                }
                "namespace_import" => {
                    if let Some(ident) = first_identifier(child) {
                        defined.insert(node_text(ident, source).to_string());
                    }
                }
                "import_specifier" => {
                    if let Some(alias) = child.child_by_field_name("alias") {
                        defined.insert(node_text(alias, source).to_string());
                    } else if let Some(name) = child.child_by_field_name("name") {
                        defined.insert(node_text(name, source).to_string());
                    }
                }
                _ => {}
            };
            walk(node, &mut vis);
        }
        "function_declaration" | "generator_function_declaration" => {
            if let Some(name) = node.child_by_field_name("name") {
                defined.insert(node_text(name, source).to_string());
            }
            if let Some(params) = node.child_by_field_name("parameters") {
                collect_js_parameters(params, source, defined);
                visit_param_annotations(params, source, defined, refs, typescript);
            }
            if let Some(body) = node.child_by_field_name("body") {
                visit(body, source, defined, refs, typescript);
            }
        }
        "class_declaration" | "class" => {
            if let Some(name) = node.child_by_field_name("name") {
                defined.insert(node_text(name, source).to_string());
            }
            if let Some(superclass) = node.child_by_field_name("superclass") {
                visit(superclass, source, defined, refs, typescript);
            }
            if let Some(body) = node.child_by_field_name("body") {
                visit(body, source, defined, refs, typescript);
            }
        }
        "interface_declaration" | "type_alias_declaration" | "enum_declaration" => {
            let name_node = node.child_by_field_name("name");
            if let Some(name) = name_node {
                defined.insert(node_text(name, source).to_string());
            }
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if name_node.map(|n| n.id()) != Some(child.id()) {
                    visit(child, source, defined, refs, typescript);
                }
            }
        }
        "variable_declarator" => {
            if let Some(value) = node.child_by_field_name("value") {
                visit(value, source, defined, refs, typescript);
            }
            if let Some(name) = node.child_by_field_name("name") {
                bind_pattern(name, source, defined, refs, typescript);
            }
        }
        "arrow_function"
        | "function"
        | "method_definition"
        | "function_expression"
        | "generator_function" => {
            if let Some(params) = node.child_by_field_name("parameters") {
                collect_js_parameters(params, source, defined);
                visit_param_annotations(params, source, defined, refs, typescript);
            } else if let Some(param) = node.child_by_field_name("parameter") {
                bind_pattern(param, source, defined, refs, typescript);
            }
            if let Some(body) = node.child_by_field_name("body") {
                visit(body, source, defined, refs, typescript);
            }
        }
        "catch_clause" => {
            if let Some(param) = node.child_by_field_name("parameter") {
                bind_pattern(param, source, defined, refs, typescript);
            }
            if let Some(body) = node.child_by_field_name("body") {
                visit(body, source, defined, refs, typescript);
            }
        }
        "for_in_statement" | "for_statement" => {
            if let Some(left) = node.child_by_field_name("left") {
                if matches!(left.kind(), "variable_declaration" | "lexical_declaration") {
                    let mut vis = |child: Node<'_>| {
                        if child.kind() == "variable_declarator" {
                            if let Some(name) = child.child_by_field_name("name") {
                                bind_pattern(name, source, defined, refs, typescript);
                            }
                        }
                    };
                    walk(left, &mut vis);
                } else {
                    bind_pattern(left, source, defined, refs, typescript);
                }
            }
            for fname in [
                "right",
                "condition",
                "update",
                "initializer",
                "increment",
                "body",
            ] {
                if let Some(child) = node.child_by_field_name(fname) {
                    visit(child, source, defined, refs, typescript);
                }
            }
        }
        "member_expression" => {
            if let Some(object) = node.child_by_field_name("object") {
                visit(object, source, defined, refs, typescript);
            }
        }
        "subscript_expression" => {
            if let Some(object) = node.child_by_field_name("object") {
                visit(object, source, defined, refs, typescript);
            }
            if let Some(index) = node.child_by_field_name("index") {
                visit(index, source, defined, refs, typescript);
            }
        }
        "property_identifier"
        | "shorthand_property_identifier"
        | "statement_identifier"
        | "label_identifier" => {}
        "pair" => {
            if let Some(value) = node.child_by_field_name("value") {
                visit(value, source, defined, refs, typescript);
            }
        }
        "jsx_attribute" => {
            if let Some(value) = node.child_by_field_name("value") {
                visit(value, source, defined, refs, typescript);
            }
        }
        "type_identifier" => {
            if typescript {
                add_reference(refs, node, source, "type");
            }
        }
        "identifier" => {
            add_reference(refs, node, source, "identifier");
        }
        _ => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                visit(child, source, defined, refs, typescript);
            }
        }
    }
}

fn collect_js_parameters(params: Node<'_>, source: &str, defined: &mut HashSet<String>) {
    let mut vis = |child: Node<'_>| match child.kind() {
        "identifier" if !is_inside_type_annotation(child) => {
            defined.insert(node_text(child, source).to_string());
        }
        "shorthand_property_identifier_pattern" => {
            defined.insert(node_text(child, source).to_string());
        }
        _ => {}
    };
    walk(params, &mut vis);
}

fn is_inside_type_annotation(node: Node<'_>) -> bool {
    let mut current = node.parent();
    while let Some(n) = current {
        match n.kind() {
            "type_annotation" | "type_parameters" | "generic_type" | "predefined_type"
            | "type_arguments" => return true,
            _ => {}
        }
        current = n.parent();
    }
    false
}

fn visit_param_annotations<'tree>(
    params: Node<'tree>,
    source: &str,
    defined: &mut HashSet<String>,
    refs: &mut Vec<UndefinedName>,
    typescript: bool,
) {
    // Collect candidate annotation/default nodes first so we don't
    // pull `defined`/`refs` aliases through the closure during the
    // walk (which would clash with the recursive `visit` borrows).
    let mut to_visit: Vec<Node<'tree>> = Vec::new();
    {
        let mut collect = |child: Node<'tree>| {
            let t = child.kind();
            if t == "type_annotation" {
                let mut cur = child.walk();
                for c in child.named_children(&mut cur) {
                    to_visit.push(c);
                }
            } else if t == "assignment_pattern" {
                if let Some(right) = child.child_by_field_name("right") {
                    to_visit.push(right);
                }
            }
        };
        walk(params, &mut collect);
    }

    for n in to_visit {
        visit(n, source, defined, refs, typescript);
    }
}
