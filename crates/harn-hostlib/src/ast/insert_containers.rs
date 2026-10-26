//! Member containers (classes, interfaces, traits, impls) that a refused
//! `ast.insert_at_anchor` offers as retry targets.
//!
//! A query that matches nothing or several nodes tells the caller little on
//! its own. Listing the containers the file has lets an agent aim the next
//! query at one of them instead of guessing at node kinds.

use harn_vm::VmValue;
use tree_sitter::{Node, Tree};

use crate::tools::args::{build_dict, str_value};

use super::language::Language;

/// Candidates listed per refusal; the rest are counted, not listed.
const MAX_CANDIDATES: usize = 50;

struct Container {
    start_byte: usize,
    line: usize,
    kind: &'static str,
    name: String,
}

/// The container kind of `node` for `language`, when it can hold methods.
fn container_kind(language: Language, node: Node<'_>) -> Option<&'static str> {
    let kind = node.kind();
    match language {
        Language::Rust => match kind {
            "impl_item" if node.child_by_field_name("trait").is_some() => Some("trait_impl"),
            "impl_item" => Some("impl"),
            "trait_item" => Some("trait"),
            _ => None,
        },
        Language::TypeScript | Language::Tsx | Language::JavaScript | Language::Jsx => match kind {
            "class_declaration" => Some("class"),
            "abstract_class_declaration" => Some("abstract_class"),
            "interface_declaration" => Some("interface"),
            _ => None,
        },
        Language::Java => match kind {
            "class_declaration" => Some("class"),
            "interface_declaration" => Some("interface"),
            "enum_declaration" => Some("enum"),
            "record_declaration" => Some("record"),
            _ => None,
        },
        Language::CSharp => match kind {
            "class_declaration" => Some("class"),
            "interface_declaration" => Some("interface"),
            "struct_declaration" => Some("struct"),
            "record_declaration" => Some("record"),
            _ => None,
        },
        Language::Python => (kind == "class_definition").then_some("class"),
        Language::Kotlin => match kind {
            "class_declaration" => Some("class"),
            "object_declaration" => Some("object"),
            _ => None,
        },
        Language::Scala => match kind {
            "class_definition" => Some("class"),
            "object_definition" => Some("object"),
            "trait_definition" => Some("trait"),
            _ => None,
        },
        Language::Swift => match kind {
            "class_declaration" => Some("class"),
            "protocol_declaration" => Some("protocol"),
            _ => None,
        },
        Language::Php => match kind {
            "class_declaration" => Some("class"),
            "interface_declaration" => Some("interface"),
            "trait_declaration" => Some("trait"),
            _ => None,
        },
        Language::Ruby => match kind {
            "class" => Some("class"),
            "module" => Some("module"),
            _ => None,
        },
        Language::Cpp => match kind {
            "class_specifier" => Some("class"),
            "struct_specifier" => Some("struct"),
            _ => None,
        },
        _ => None,
    }
}

fn describe(language: Language, node: Node<'_>, source: &[u8]) -> Option<Container> {
    let kind = container_kind(language, node)?;
    let name_field = if language == Language::Rust && kind != "trait" {
        "type"
    } else {
        "name"
    };
    // Grammars without a `name` field (Kotlin) put the identifier first.
    let mut cursor = node.walk();
    let named = node.child_by_field_name(name_field).or_else(|| {
        node.named_children(&mut cursor).find(|child| {
            matches!(
                child.kind(),
                "type_identifier" | "simple_identifier" | "identifier"
            )
        })
    });
    let name = named
        .and_then(|n| n.utf8_text(source).ok())
        .unwrap_or_default()
        .to_string();
    Some(Container {
        start_byte: node.start_byte(),
        line: node.start_position().row + 1,
        kind,
        name,
    })
}

fn value(path: &str, container: &Container) -> VmValue {
    build_dict([
        ("path", str_value(path)),
        ("line", VmValue::Int(container.line as i64)),
        ("kind", str_value(container.kind)),
        ("name", str_value(&container.name)),
    ])
}

/// Candidates, whether the list was cut at [`MAX_CANDIDATES`], and a one-line
/// reading of the first few for callers that only forward `details`.
pub(super) struct Candidates {
    pub values: Vec<VmValue>,
    pub truncated: bool,
    pub summary: String,
}

/// Candidates named in `details`; the structured list carries the rest.
const SUMMARY_CANDIDATES: usize = 8;

fn finish(path: &str, mut found: Vec<Container>) -> Candidates {
    found.sort_by_key(|c| c.start_byte);
    found.dedup_by_key(|c| c.start_byte);
    let truncated = found.len() > MAX_CANDIDATES;
    found.truncate(MAX_CANDIDATES);
    let mut summary: Vec<String> = found
        .iter()
        .take(SUMMARY_CANDIDATES)
        .map(|c| format!("{} {} (line {})", c.kind, c.name, c.line))
        .collect();
    if found.len() > SUMMARY_CANDIDATES {
        summary.push(format!("{} more", found.len() - SUMMARY_CANDIDATES));
    }
    Candidates {
        values: found.iter().map(|c| value(path, c)).collect(),
        truncated,
        summary: summary.join(", "),
    }
}

/// Every container in the file: what a query that matched nothing could have named.
pub(super) fn in_file(language: Language, tree: &Tree, source: &[u8], path: &str) -> Candidates {
    let mut found = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if let Some(container) = describe(language, node, source) {
            found.push(container);
        }
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                stack.push(child);
            }
        }
    }
    finish(path, found)
}

/// The container around each anchor span (the anchor's own node when it is
/// one): what several matches turn out to be.
pub(super) fn around(
    language: Language,
    tree: &Tree,
    source: &[u8],
    path: &str,
    spans: &[(usize, usize)],
) -> Candidates {
    let mut found = Vec::new();
    for &(start, end) in spans {
        let mut node = tree.root_node().descendant_for_byte_range(start, end);
        while let Some(current) = node {
            if let Some(container) = describe(language, current, source) {
                found.push(container);
                break;
            }
            node = current.parent();
        }
    }
    finish(path, found)
}

#[cfg(test)]
mod tests {
    use super::super::insert_at_anchor::run;
    use crate::test_support::*;
    use harn_vm::VmValue;

    const RUST: &str = "pub trait Shape {\n    fn area(&self) -> f64;\n}\n\npub struct Square(f64);\n\nimpl Square {\n    pub fn side(&self) -> f64 {\n        self.0\n    }\n}\n\nimpl Shape for Square {\n    fn area(&self) -> f64 {\n        self.0 * self.0\n    }\n}\n";

    const METHOD: &str = "fn added(&self) -> u8 {\n    0\n}";

    const TRAIT_QUERY: &str = "(trait_item name: (type_identifier) @name body: (declaration_list) @anchor (#eq? @name \"Shape\"))";
    const INHERENT_QUERY: &str = "(impl_item !trait type: (type_identifier) @name body: (declaration_list) @anchor (#eq? @name \"Square\"))";

    fn insert(extension: &str, source: &str, query: &str, content: &str) -> VmValue {
        let file = write_temp(extension, source);
        let path = file.path().to_string_lossy().to_string();
        let result = run(&[dict(&[
            ("path", vm_string(&path)),
            ("query", vm_string(query)),
            ("position", vm_string("last_child")),
            ("content", vm_string(content)),
            ("dry_run", VmValue::Bool(true)),
        ])])
        .expect("insert_at_anchor runs");
        crate::schemas::validate_response("ast.insert_at_anchor", "ast", "insert_at_anchor", result)
            .expect("retry targets satisfy the public response contract")
    }

    fn candidates(result: &VmValue) -> Vec<(i64, String, String)> {
        let VmValue::List(list) = field(result, "candidates") else {
            panic!("candidates must be a list: {result:?}");
        };
        list.iter()
            .map(|c| {
                (
                    n(field(c, "line")),
                    s(field(c, "kind")),
                    s(field(c, "name")),
                )
            })
            .collect()
    }

    #[test]
    fn a_trait_definition_is_an_anchor_and_leaves_the_impls_alone() {
        let result = insert("rs", RUST, TRAIT_QUERY, METHOD);
        assert_eq!(s(field(&result, "result")), "applied", "{result:?}");
        let preview = s(field(&result, "preview"));
        let added = preview.find("fn added").expect("method inserted");
        let end_of_trait = preview.find("pub struct Square").unwrap();
        assert!(added < end_of_trait, "inside the trait:\n{preview}");
        assert_eq!(preview.matches("fn added").count(), 1, "{preview}");
    }

    #[test]
    fn an_inherent_impl_anchor_is_unchanged_and_skips_the_trait_and_trait_impl() {
        let result = insert("rs", RUST, INHERENT_QUERY, METHOD);
        assert_eq!(s(field(&result, "result")), "applied", "{result:?}");
        let preview = s(field(&result, "preview"));
        let added = preview.find("fn added").expect("method inserted");
        let inherent = preview.find("impl Square").unwrap();
        let trait_impl = preview.find("impl Shape for Square").unwrap();
        assert!(inherent < added && added < trait_impl, "{preview}");
    }

    #[test]
    fn an_empty_match_lists_every_container_in_the_file() {
        let result = insert(
            "rs",
            RUST,
            "(trait_item name: (type_identifier) @name body: (declaration_list) @anchor (#eq? @name \"Missing\"))",
            METHOD,
        );
        assert_eq!(s(field(&result, "result")), "no_match");
        assert_eq!(
            candidates(&result),
            vec![
                (1, "trait".into(), "Shape".into()),
                (7, "impl".into(), "Square".into()),
                (13, "trait_impl".into(), "Square".into()),
            ]
        );
        // Callers that forward only `details` still see the retry targets.
        assert_eq!(
            s(field(&result, "details")),
            "query produced zero anchors; containers: trait Shape (line 1), impl Square (line 7), trait_impl Square (line 13)"
        );
    }

    #[test]
    fn several_matches_list_the_containers_they_sit_in() {
        let result = insert("rs", RUST, "(declaration_list) @anchor", METHOD);
        assert_eq!(s(field(&result, "result")), "ambiguous");
        assert_eq!(n(field(&result, "match_count")), 3);
        assert_eq!(
            candidates(&result),
            vec![
                (1, "trait".into(), "Shape".into()),
                (7, "impl".into(), "Square".into()),
                (13, "trait_impl".into(), "Square".into()),
            ]
        );
    }

    #[test]
    fn typescript_classes_and_interfaces_are_candidates() {
        let source = "interface Named {\n  name: string\n}\nclass A {}\nabstract class B {}\n";
        let result = insert(
            "ts",
            source,
            "(class_declaration name: (type_identifier) @name body: (class_body) @anchor (#eq? @name \"Nope\"))",
            "m(): number { return 0 }",
        );
        assert_eq!(s(field(&result, "result")), "no_match");
        assert_eq!(
            candidates(&result),
            vec![
                (1, "interface".into(), "Named".into()),
                (4, "class".into(), "A".into()),
                (5, "abstract_class".into(), "B".into()),
            ]
        );
    }

    #[test]
    fn candidates_stop_at_the_cap_and_say_so() {
        let source: String = (0..60).map(|i| format!("class C{i} {{}}\n")).collect();
        let result = insert(
            "ts",
            &source,
            "(class_declaration name: (type_identifier) @name body: (class_body) @anchor (#eq? @name \"Nope\"))",
            "m(): number { return 0 }",
        );
        assert_eq!(candidates(&result).len(), super::MAX_CANDIDATES);
        assert!(b(field(&result, "candidates_truncated")));
    }

    #[test]
    fn every_language_with_classes_lists_them() {
        let cases: [(&str, &str, &str, &str, &str); 6] = [
            (
                "kt",
                "interface Named {\n}\nclass A {\n}\nobject B {\n}\n",
                "((_) @anchor (#eq? @anchor \"nope\"))",
                "class",
                "A",
            ),
            (
                "scala",
                "trait Named {}\nclass A {}\nobject B {}\n",
                "((_) @anchor (#eq? @anchor \"nope\"))",
                "class",
                "A",
            ),
            (
                "php",
                "<?php\ninterface Named {}\nclass A {}\ntrait T {}\n",
                "((_) @anchor (#eq? @anchor \"nope\"))",
                "class",
                "A",
            ),
            (
                "rb",
                "class A\nend\nmodule M\nend\n",
                "((_) @anchor (#eq? @anchor \"nope\"))",
                "class",
                "A",
            ),
            (
                "swift",
                "protocol Named {}\nclass A {}\n",
                "((_) @anchor (#eq? @anchor \"nope\"))",
                "class",
                "A",
            ),
            (
                "cpp",
                "class A {};\nstruct S {};\n",
                "((_) @anchor (#eq? @anchor \"nope\"))",
                "class",
                "A",
            ),
        ];
        for (extension, source, query, kind, name) in cases {
            let result = insert(extension, source, query, "x");
            assert_eq!(
                s(field(&result, "result")),
                "no_match",
                "{extension}: {result:?}"
            );
            let found = candidates(&result);
            assert!(
                found.iter().any(|(_, k, n)| k == kind && n == name),
                "{extension}: {found:?}"
            );
            assert!(found.len() >= 2, "{extension}: {found:?}");
        }
    }
}
