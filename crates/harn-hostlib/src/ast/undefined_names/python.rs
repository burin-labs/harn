//! Python profile for \`ast.undefined_names\`.

use super::*;

pub(super) static BUILTINS: std::sync::LazyLock<HashSet<&'static str>> =
    std::sync::LazyLock::new(|| {
        [
            "__name__",
            "__main__",
            "__file__",
            "__doc__",
            "__builtins__",
            "__dict__",
            "__init__",
            "__class__",
            "__all__",
            "__author__",
            "__version__",
            "True",
            "False",
            "None",
            "NotImplemented",
            "Ellipsis",
            "self",
            "cls",
            "super",
            "abs",
            "all",
            "any",
            "ascii",
            "bin",
            "bool",
            "breakpoint",
            "bytearray",
            "bytes",
            "callable",
            "chr",
            "classmethod",
            "compile",
            "complex",
            "delattr",
            "dict",
            "dir",
            "divmod",
            "enumerate",
            "eval",
            "exec",
            "exit",
            "filter",
            "float",
            "format",
            "frozenset",
            "getattr",
            "globals",
            "hasattr",
            "hash",
            "help",
            "hex",
            "id",
            "input",
            "int",
            "isinstance",
            "issubclass",
            "iter",
            "len",
            "list",
            "locals",
            "map",
            "max",
            "memoryview",
            "min",
            "next",
            "object",
            "oct",
            "open",
            "ord",
            "pow",
            "print",
            "property",
            "range",
            "repr",
            "reversed",
            "round",
            "set",
            "setattr",
            "slice",
            "sorted",
            "staticmethod",
            "str",
            "sum",
            "tuple",
            "type",
            "vars",
            "zip",
            "Exception",
            "BaseException",
            "ValueError",
            "TypeError",
            "KeyError",
            "IndexError",
            "AttributeError",
            "RuntimeError",
            "StopIteration",
            "NotImplementedError",
            "FileNotFoundError",
            "IOError",
            "OSError",
            "ArithmeticError",
            "ZeroDivisionError",
            "OverflowError",
            "NameError",
            "UnicodeDecodeError",
            "UnicodeEncodeError",
            "ImportError",
            "ModuleNotFoundError",
            "GeneratorExit",
            "KeyboardInterrupt",
            "SystemExit",
            "match",
            "case",
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
        "function_definition" | "lambda" => {
            if node.kind() == "function_definition" {
                if let Some(name) = node.child_by_field_name("name") {
                    defined.insert(node_text(name, source).to_string());
                }
            }
            if let Some(params) = node.child_by_field_name("parameters") {
                collect_python_parameters(params, source, defined);
                // Defaults are *references*: walk the parameter list
                // and visit any default value node we find.
                let mut visit_defaults = |child: Node<'_>| {
                    let t = child.kind();
                    let has_default = t == "default_parameter" || t == "typed_default_parameter";
                    if has_default {
                        if let Some(value) = child.child_by_field_name("value") {
                            visit(value, source, defined, refs);
                        }
                    }
                };
                walk(params, &mut visit_defaults);
            }
            if let Some(rt) = node.child_by_field_name("return_type") {
                visit(rt, source, defined, refs);
            }
            if let Some(body) = node.child_by_field_name("body") {
                visit(body, source, defined, refs);
            }
        }
        "class_definition" => {
            if let Some(name) = node.child_by_field_name("name") {
                defined.insert(node_text(name, source).to_string());
            }
            if let Some(bases) = node.child_by_field_name("superclasses") {
                visit(bases, source, defined, refs);
            }
            if let Some(body) = node.child_by_field_name("body") {
                visit(body, source, defined, refs);
            }
        }
        "import_statement" | "import_from_statement" => {
            collect_python_imports(node, source, defined);
        }
        "for_statement" | "for_in_clause" => {
            if let Some(target) = node.child_by_field_name("left") {
                collect_python_targets(target, source, defined);
            }
            if let Some(right) = node.child_by_field_name("right") {
                visit(right, source, defined, refs);
            }
            if let Some(body) = node.child_by_field_name("body") {
                visit(body, source, defined, refs);
            }
        }
        "assignment" => {
            if let Some(right) = node.child_by_field_name("right") {
                visit(right, source, defined, refs);
            }
            if let Some(left) = node.child_by_field_name("left") {
                collect_python_targets(left, source, defined);
            }
        }
        "named_expression" => {
            if let Some(value) = node.child_by_field_name("value") {
                visit(value, source, defined, refs);
            }
            if let Some(name) = node.child_by_field_name("name") {
                defined.insert(node_text(name, source).to_string());
            }
        }
        "global_statement" | "nonlocal_statement" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if child.kind() == "identifier" {
                    defined.insert(node_text(child, source).to_string());
                }
            }
        }
        "with_statement" => {
            let mut cursor = node.walk();
            for item in node.named_children(&mut cursor) {
                let mut vis = |child: Node<'_>| {
                    if child.kind() == "as_pattern_target" {
                        if let Some(ident) = first_identifier(child) {
                            defined.insert(node_text(ident, source).to_string());
                        }
                    }
                };
                walk(item, &mut vis);
                if let Some(value) = item.child_by_field_name("value") {
                    visit(value, source, defined, refs);
                }
            }
        }
        "except_clause" => {
            let mut saw_as = false;
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "as" {
                    saw_as = true;
                    continue;
                }
                if saw_as && child.kind() == "identifier" {
                    defined.insert(node_text(child, source).to_string());
                    saw_as = false;
                } else if child.is_named() {
                    visit(child, source, defined, refs);
                }
            }
        }
        "attribute" => {
            if let Some(object) = node.child_by_field_name("object") {
                visit(object, source, defined, refs);
            }
        }
        "keyword_argument" => {
            if let Some(value) = node.child_by_field_name("value") {
                visit(value, source, defined, refs);
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

fn collect_python_parameters(params: Node<'_>, source: &str, defined: &mut HashSet<String>) {
    let mut cursor = params.walk();
    for child in params.named_children(&mut cursor) {
        match child.kind() {
            "identifier" => {
                defined.insert(node_text(child, source).to_string());
            }
            "typed_parameter"
            | "default_parameter"
            | "typed_default_parameter"
            | "list_splat_pattern"
            | "dictionary_splat_pattern" => {
                if let Some(name) = child.child_by_field_name("name") {
                    defined.insert(node_text(name, source).to_string());
                } else if let Some(ident) = first_identifier(child) {
                    defined.insert(node_text(ident, source).to_string());
                }
            }
            _ => {
                if let Some(ident) = first_identifier(child) {
                    defined.insert(node_text(ident, source).to_string());
                }
            }
        }
    }
}

fn collect_python_imports(node: Node<'_>, source: &str, defined: &mut HashSet<String>) {
    // Bind aliases and bare module names from anywhere in the import.
    let mut vis = |child: Node<'_>| match child.kind() {
        "aliased_import" => {
            if let Some(alias) = child.child_by_field_name("alias") {
                defined.insert(node_text(alias, source).to_string());
            }
        }
        "dotted_name" => {
            if let Some(first) = child.named_child(0) {
                defined.insert(node_text(first, source).to_string());
            }
        }
        _ => {}
    };
    walk(node, &mut vis);

    // `from x import y, z` — direct identifiers after `import`.
    if node.kind() == "import_from_statement" {
        let mut saw_import = false;
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "import" {
                saw_import = true;
                continue;
            }
            if saw_import {
                if child.kind() == "identifier" {
                    defined.insert(node_text(child, source).to_string());
                } else if child.kind() == "dotted_name" {
                    let count = child.named_child_count();
                    if count > 0 {
                        if let Some(last) = child.named_child((count - 1) as u32) {
                            defined.insert(node_text(last, source).to_string());
                        }
                    }
                }
            }
        }
    }
}

fn collect_python_targets(node: Node<'_>, source: &str, defined: &mut HashSet<String>) {
    match node.kind() {
        "identifier" => {
            defined.insert(node_text(node, source).to_string());
        }
        "pattern_list" | "tuple_pattern" | "list_pattern" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                collect_python_targets(child, source, defined);
            }
        }
        "list_splat_pattern" => {
            if let Some(ident) = first_identifier(node) {
                defined.insert(node_text(ident, source).to_string());
            }
        }
        // Assignment to `a.b` / `a[b]` doesn't bind a new local.
        "attribute" | "subscript" => {}
        _ => {}
    }
}
