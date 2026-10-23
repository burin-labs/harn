//! Check an inserted Java method against its direct siblings before a host writes it.

use std::collections::BTreeSet;

use harn_vm::VmValue;
use tree_sitter::Node;

use crate::error::HostlibError;
use crate::tools::args::{build_dict, dict_arg, optional_int, require_string, str_value};

use super::language::Language;
use super::parse::{parse_source, ContentOrPath};
use super::symbols::helpers::{children, node_text};

const BUILTIN: &str = "hostlib_ast_check_member_insertion";

pub(super) fn run(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let input = ContentOrPath::from_args(BUILTIN, args)?;
    let raw = dict_arg(BUILTIN, args)?;
    let name = require_string(BUILTIN, &raw, "name")?;
    let container_line = positive_line(&raw, "container_line")?;
    let start_line = positive_line(&raw, "start_line")?;
    let end_line = positive_line(&raw, "end_line")?;
    if end_line < start_line {
        return Err(HostlibError::InvalidParameter {
            builtin: BUILTIN,
            param: "end_line",
            message: "must be at least start_line".into(),
        });
    }
    if input.language != Language::Java {
        return Ok(verdict(
            "unsupported",
            "method identity is unavailable for this language",
            0,
            vec![],
        ));
    }
    let source = input.load_source()?;
    // Java expands these before tokenization, so a raw tree can hide declarations in comments.
    if source.contains("\\u") {
        return Ok(verdict(
            "unresolved",
            "Java Unicode escapes require compiler resolution",
            0,
            vec![],
        ));
    }
    let tree = parse_source(&source, input.language)?;
    if tree.root_node().has_error() {
        return Ok(verdict(
            "unresolved",
            "source contains parse errors",
            0,
            vec![],
        ));
    }
    let mut containers = Vec::new();
    find_containers(tree.root_node(), container_line, &mut containers);
    let [container] = containers.as_slice() else {
        return Ok(verdict(
            "unresolved",
            "container line must identify one type",
            0,
            vec![],
        ));
    };
    let Some(body) = container.child_by_field_name("body") else {
        return Ok(verdict("unresolved", "type has no member body", 0, vec![]));
    };
    let inserted: Vec<_> = children(body)
        .filter(|node| node.is_named() && !matches!(node.kind(), "line_comment" | "block_comment"))
        .filter(|node| (start_line..=end_line).contains(&(node.start_position().row + 1)))
        .collect();
    let [method] = inserted.as_slice() else {
        return Ok(verdict(
            "unresolved",
            "inserted range must contain exactly one member",
            0,
            vec![],
        ));
    };
    if method.kind() != "method_declaration"
        || method.end_position().row + 1 > end_line
        || method
            .child_by_field_name("name")
            .map(|n| node_text(n, &source))
            .as_deref()
            != Some(name.as_str())
    {
        return Ok(verdict(
            "unresolved",
            "inserted member must be the requested method",
            0,
            vec![],
        ));
    }
    let Some(parameters) = method.child_by_field_name("parameters") else {
        return Ok(verdict(
            "unresolved",
            "method parameters are unavailable",
            0,
            vec![],
        ));
    };
    let candidate = parameter_types(parameters, *method, &source);
    let mut compared = 0;
    let mut unresolved = false;
    let mut conflicts = Vec::new();
    for sibling in children(body)
        .filter(|node| node.kind() == "method_declaration" && node.id() != method.id())
    {
        if sibling
            .child_by_field_name("name")
            .map(|n| node_text(n, &source))
            .as_deref()
            != Some(name.as_str())
        {
            continue;
        }
        compared += 1;
        let Some(parameters) = sibling.child_by_field_name("parameters") else {
            unresolved = true;
            continue;
        };
        let other = parameter_types(parameters, sibling, &source);
        if candidate.len() != other.len() {
            continue;
        }
        // One proven different parameter is enough; unknown types cannot prove equality or difference.
        if candidate.iter().zip(&other).any(|(a, b)| match (a, b) {
            (Some(a), Some(b)) => a.rsplit('.').next() != b.rsplit('.').next(),
            _ => false,
        }) {
            continue;
        }
        if candidate.iter().chain(&other).any(Option::is_none) || candidate != other {
            unresolved = true;
        } else {
            conflicts.push(build_dict([
                ("name", str_value(&name)),
                (
                    "line",
                    VmValue::Int((sibling.start_position().row + 1) as i64),
                ),
            ]));
        }
    }
    if !conflicts.is_empty() {
        Ok(verdict(
            "duplicate",
            "method has the same erased parameter types as a sibling",
            compared,
            conflicts,
        ))
    } else if unresolved {
        Ok(verdict(
            "unresolved",
            "parameter types cannot be distinguished safely",
            compared,
            vec![],
        ))
    } else {
        Ok(verdict(
            "clear",
            "no conflicting direct method",
            compared,
            vec![],
        ))
    }
}

fn positive_line(
    dict: &harn_vm::value::DictMap,
    name: &'static str,
) -> Result<usize, HostlibError> {
    let value = optional_int(BUILTIN, dict, name, 0)?;
    usize::try_from(value)
        .ok()
        .filter(|line| *line > 0)
        .ok_or_else(|| HostlibError::InvalidParameter {
            builtin: BUILTIN,
            param: name,
            message: "must be a positive 1-based line".into(),
        })
}

fn verdict(status: &str, reason: &str, compared: i64, conflicts: Vec<VmValue>) -> VmValue {
    build_dict([
        ("status", str_value(status)),
        ("reason", str_value(reason)),
        ("compared_members", VmValue::Int(compared)),
        ("conflicts", VmValue::list(conflicts)),
    ])
}

fn find_containers<'tree>(node: Node<'tree>, line: usize, out: &mut Vec<Node<'tree>>) {
    if node.start_position().row + 1 == line
        && matches!(
            node.kind(),
            "class_declaration"
                | "interface_declaration"
                | "record_declaration"
                | "enum_declaration"
        )
    {
        out.push(node);
    }
    for child in children(node).filter(Node::is_named) {
        find_containers(child, line, out);
    }
}

fn parameter_types(parameters: Node<'_>, method: Node<'_>, source: &str) -> Vec<Option<String>> {
    let mut variables = BTreeSet::new();
    let mut scope = Some(method);
    while let Some(node) = scope {
        if let Some(parameters) = node.child_by_field_name("type_parameters") {
            for parameter in children(parameters).filter(|n| n.kind() == "type_parameter") {
                if let Some(name) = children(parameter).find(|n| n.kind() == "type_identifier") {
                    variables.insert(node_text(name, source));
                }
            }
        }
        scope = node.parent();
    }
    children(parameters)
        .filter(|node| {
            node.is_named()
                && !matches!(
                    node.kind(),
                    "receiver_parameter" | "line_comment" | "block_comment"
                )
        })
        .map(|parameter| {
            let (ty, extra_dimensions) = match parameter.kind() {
                "formal_parameter" => (
                    parameter.child_by_field_name("type")?,
                    parameter
                        .child_by_field_name("dimensions")
                        .map_or(0, dimension_count),
                ),
                "spread_parameter" => (children(parameter).find(|node| is_type(*node))?, 1),
                _ => return None,
            };
            let mut erased = erased_type(ty, source, &variables)?;
            erased.push_str(&"[]".repeat(extra_dimensions));
            Some(erased)
        })
        .collect()
}

fn dimension_count(node: Node<'_>) -> usize {
    children(node).filter(|child| child.kind() == "[").count()
}

fn is_type(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "integral_type"
            | "floating_point_type"
            | "boolean_type"
            | "type_identifier"
            | "scoped_type_identifier"
            | "generic_type"
            | "array_type"
            | "annotated_type"
    )
}

fn erased_type(node: Node<'_>, source: &str, variables: &BTreeSet<String>) -> Option<String> {
    match node.kind() {
        "integral_type" | "floating_point_type" | "boolean_type" => Some(node_text(node, source)),
        "type_identifier" => {
            let name = node_text(node, source);
            (!variables.contains(&name)).then_some(name)
        }
        "scoped_type_identifier" => children(node)
            .filter(|n| is_type(*n))
            .map(|n| erased_type(n, source, variables))
            .collect::<Option<Vec<_>>>()
            .map(|parts| parts.join(".")),
        "generic_type" | "annotated_type" => children(node)
            .find(|n| is_type(*n))
            .and_then(|n| erased_type(n, source, variables)),
        "array_type" => {
            let mut element = erased_type(node.child_by_field_name("element")?, source, variables)?;
            element
                .push_str(&"[]".repeat(dimension_count(node.child_by_field_name("dimensions")?)));
            Some(element)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{dict, field, n, s};

    fn check(header: &str, before: &str, inserted: &str, after: &str) -> VmValue {
        let source = format!("{header} {{\n{before}\n{inserted}\n}}\n{after}\n");
        run(&[dict(&[
            ("content", str_value(source)),
            ("language", str_value("java")),
            ("name", str_value("read")),
            ("container_line", VmValue::Int(1)),
            ("start_line", VmValue::Int(3)),
            ("end_line", VmValue::Int(3)),
        ])])
        .expect("member check")
    }

    #[test]
    fn java_method_insertion_distinguishes_overloads_and_duplicates() {
        let cases = [
            (
                "int read(int value) { return 1; }",
                "int read(int value, boolean strict) { return 2; }",
                "clear",
            ),
            (
                "int read(int value) { return 1; }",
                "int read(String value) { return 2; }",
                "clear",
            ),
            (
                "int read(int value) { return 1; }",
                "long read(int renamed) { return 2; }",
                "duplicate",
            ),
            (
                "int read(java.util.List<String> value) { return 1; }",
                "int read(java.util.List<Integer> renamed) { return 2; }",
                "duplicate",
            ),
            (
                "int read(String[] value) { return 1; }",
                "int read(String... renamed) { return 2; }",
                "duplicate",
            ),
            (
                "int read(int value[]) { return 1; }",
                "int read(int[] renamed) { return 2; }",
                "duplicate",
            ),
            (
                "int read(int[] value) { return 1; }",
                "int read(int[][] renamed) { return 2; }",
                "clear",
            ),
            (
                "int read(@Deprecated final String value) { return 1; }",
                "int read(String renamed) { return 2; }",
                "duplicate",
            ),
            (
                "int read(String value) { return 1; }",
                "int read(java.lang.String renamed) { return 2; }",
                "unresolved",
            ),
            (
                "<T> int read(T value) { return 1; }",
                "int read(Object renamed) { return 2; }",
                "unresolved",
            ),
        ];
        for (before, inserted, expected) in cases {
            let result = check("class Store", before, inserted, "");
            assert_eq!(
                s(field(&result, "status")),
                expected,
                "{before}\n{inserted}\n{result:?}"
            );
            assert_eq!(n(field(&result, "compared_members")), 1);
        }
        let sibling = check(
            "class Store",
            "",
            "int read(int value) { return 1; }",
            "class Other { int read(int value) { return 2; } }",
        );
        assert_eq!(s(field(&sibling, "status")), "clear");
        assert_eq!(n(field(&sibling, "compared_members")), 0);
        let nested = check(
            "class Store",
            "class Other { int read(int value) { return 2; } }",
            "int read(int value) { return 1; }",
            "",
        );
        assert_eq!(s(field(&nested, "status")), "clear");
        let generic = check(
            "class Store<T>",
            "int read(T value) { return 1; }",
            "int read(Object value) { return 2; }",
            "",
        );
        assert_eq!(s(field(&generic, "status")), "unresolved");
        let missing = check("class Store", "", "int other(int value) { return 1; }", "");
        assert_eq!(s(field(&missing, "status")), "unresolved");
        let escaped = check(
            "class Store",
            "// \\u000a int read(int key) { return 1; }",
            "int read(int value) { return 2; }",
            "",
        );
        assert_eq!(s(field(&escaped, "status")), "unresolved");
        let injected = check(
            "class Store",
            "",
            "int read(int value) { return 1; } int extra() { return 2; }",
            "",
        );
        assert_eq!(s(field(&injected, "status")), "unresolved");
    }
}
