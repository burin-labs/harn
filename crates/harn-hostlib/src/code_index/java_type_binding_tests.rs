use super::{descendants, text, JavaBindings, JavaTypeCatalog};
use crate::ast::{api::parse_tree, Language};

const TARGET: &str = "package lab; public class DocumentStore { public DocumentStore() {} static int size() { return 1; } }";
const OTHER: &str = "package other; public class DocumentStore {}";

fn catalog(sources: &[&str]) -> JavaTypeCatalog {
    let mut catalog = JavaTypeCatalog::default();
    for source in sources {
        let tree = parse_tree(source, Language::Java).unwrap();
        assert!(!tree.root_node().has_error(), "{source}");
        catalog
            .add(&JavaBindings::new(&tree, source), tree.root_node(), None)
            .unwrap();
    }
    catalog
}

#[test]
fn type_namespace_preserves_imports_nested_types_parameters_and_namesakes() {
    for (source, expected) in [
        ("package lab; class Page { DocumentStore first; DocumentStore[] second = new DocumentStore[0]; }", 3),
        ("package app; import lab.DocumentStore; class Page { DocumentStore field = new DocumentStore(); }", 3),
        ("package app; class Page { lab.DocumentStore field = new lab.DocumentStore(); }", 2),
        ("package app; import other.DocumentStore; class Page { DocumentStore field; }", 0),
        ("package lab; class Page<DocumentStore> { DocumentStore field; }", 0),
        ("package lab; class Page { static class DocumentStore {} DocumentStore field; }", 0),
        ("package lab; class Page<DocumentStore> { void use() { class DocumentStore {} DocumentStore local; } }", 0),
        ("package lab; class Page { void use() { class DocumentStore {} DocumentStore local; } }", 0),
        ("package lab; class Page { int use(DocumentStore value) { int DocumentStore = 1; return DocumentStore; } }", 1),
        ("package lab; class Page { int use() { return DocumentStore.size(); } }", 1),
        ("package lab; class Holder { int size() { return 42; } } class Outer { Holder DocumentStore = new Holder(); class Inner { int use() { return DocumentStore.size(); } } }", 0),
        ("package lab; class Holder { int size() { return 42; } } record Row(Holder DocumentStore) { int use() { return DocumentStore.size(); } }", 0),
        ("package lab; enum Choice { DocumentStore; int size() { return 43; } int use() { return DocumentStore.size(); } }", 0),
        ("package lab; class Holder { int size() { return 42; } } interface Page { Holder DocumentStore = new Holder(); default int use() { return DocumentStore.size(); } }", 0),
    ] {
        let catalog = catalog(&[TARGET, OTHER, source]);
        let tree = parse_tree(source, Language::Java).unwrap();
        let bindings = JavaBindings::new(&tree, source);
        let mut selected = 0;
        for node in descendants(tree.root_node()).filter(|node| {
            matches!(node.kind(), "identifier" | "type_identifier") && text(*node, source) == "DocumentStore"
        }) {
            selected += usize::from(bindings.type_occurrence(node, &catalog).unwrap().as_deref() == Some("lab.DocumentStore"));
        }
        assert_eq!(selected, expected, "{source}");
    }
}

#[test]
fn nested_type_and_constructor_identity_share_one_declared_owner() {
    let source = "package lab; class Outer { static class DocumentStore { DocumentStore() {} void DocumentStore() {} } DocumentStore field; }";
    let catalog = catalog(&[source]);
    let tree = parse_tree(source, Language::Java).unwrap();
    let bindings = JavaBindings::new(&tree, source);
    let identities = descendants(tree.root_node())
        .filter(|node| {
            matches!(node.kind(), "identifier" | "type_identifier")
                && text(*node, source) == "DocumentStore"
        })
        .map(|node| bindings.type_occurrence(node, &catalog).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        identities
            .iter()
            .filter(|identity| identity.as_deref() == Some("lab.Outer.DocumentStore"))
            .count(),
        3
    );
    assert_eq!(
        identities
            .iter()
            .filter(|identity| identity.is_none())
            .count(),
        1,
        "the same-named method is not a constructor"
    );
    let reference = descendants(tree.root_node())
        .filter(|node| node.kind() == "type_identifier" && text(*node, source) == "DocumentStore")
        .last()
        .unwrap();
    assert!(!bindings
        .type_name_capture(reference, "DocumentLibrary", &catalog)
        .unwrap());
}

#[test]
fn qualified_member_types_require_a_proven_declaring_owner() {
    let base =
        "package lab; class Base { static class DocumentStore {} } class Sub extends Base {}";
    for (source, expected) in [
        ("package lab; class Page { Base.DocumentStore field; }", Some("lab.Base.DocumentStore")),
        ("package lab; class Page { Sub.DocumentStore field; }", None),
        ("package app; import lab.Sub.DocumentStore; class Page { DocumentStore field; }", None),
        ("package app; class Page { ext.Sub.DocumentStore field; }", None),
        ("package app; import ext.Sub.DocumentStore; class Page { DocumentStore field; }", None),
        ("package lab; class Page { void use() { class Local extends Base {} Local.DocumentStore field; } }", None),
    ] {
        let catalog = catalog(&[base, source]);
        let tree = parse_tree(source, Language::Java).unwrap();
        let bindings = JavaBindings::new(&tree, source);
        let occurrences = descendants(tree.root_node()).filter(|node| {
            matches!(node.kind(), "identifier" | "type_identifier") && text(*node, source) == "DocumentStore"
        }).collect::<Vec<_>>();
        assert!(!occurrences.is_empty());
        for node in occurrences {
            let actual = bindings.type_occurrence(node, &catalog);
            if let Some(expected) = expected {
                assert_eq!(actual.unwrap().as_deref(), Some(expected), "{source}");
            } else {
                assert!(actual.is_err(), "an inherited member must refuse: {source}: {actual:?}");
            }
        }
    }
}

#[test]
fn later_local_declarators_do_not_shadow_an_earlier_initializer() {
    let source = "package lab; class DocumentStore { static Holder make() { return new Holder(); } } class Holder {} class Page { void use() { Holder first = DocumentStore.make(), DocumentStore = new Holder(); DocumentStore.toString(); } }";
    let catalog = catalog(&[source]);
    let tree = parse_tree(source, Language::Java).unwrap();
    let bindings = JavaBindings::new(&tree, source);
    let mut receiver_nodes = descendants(tree.root_node())
        .filter(|node| {
            node.kind() == "identifier"
                && text(*node, source) == "DocumentStore"
                && node
                    .parent()
                    .is_some_and(|parent| parent.kind() == "method_invocation")
        })
        .collect::<Vec<_>>();
    receiver_nodes.sort_by_key(|node| node.start_byte());
    let receivers = receiver_nodes
        .into_iter()
        .map(|node| bindings.type_occurrence(node, &catalog).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(receivers, [Some("lab.DocumentStore".into()), None]);
}

#[test]
fn implicit_static_fields_compete_with_nested_types_in_single_imports() {
    for owner in [
        "package lab; interface Owner { int DocumentStore = 1; class DocumentStore {} }",
        "package lab; enum Owner { DocumentStore; static class DocumentStore {} }",
    ] {
        let source = "package app; import static lab.Owner.DocumentStore; class Page { Object value = DocumentStore; }";
        let catalog = catalog(&[owner, source]);
        let tree = parse_tree(source, Language::Java).unwrap();
        let bindings = JavaBindings::new(&tree, source);
        let import = descendants(tree.root_node())
            .find(|node| {
                node.kind() == "identifier"
                    && text(*node, source) == "DocumentStore"
                    && std::iter::successors(node.parent(), |node| node.parent())
                        .any(|node| node.kind() == "import_declaration")
            })
            .unwrap();
        assert!(
            bindings.type_occurrence(import, &catalog).is_err(),
            "{owner}"
        );
    }
}

#[test]
fn a_nearer_local_type_shadows_an_outer_type_parameter() {
    let source = "package lab; class Page<DocumentStore> { void use() { class DocumentStore {} DocumentStore local; } }";
    let catalog = catalog(&[TARGET, source]);
    let tree = parse_tree(source, Language::Java).unwrap();
    let bindings = JavaBindings::new(&tree, source);
    let reference = descendants(tree.root_node())
        .filter(|node| node.kind() == "type_identifier" && text(*node, source) == "DocumentStore")
        .max_by_key(|node| node.start_byte())
        .unwrap();
    let actual = bindings
        .type_occurrence(reference, &catalog)
        .unwrap()
        .unwrap();
    assert!(
        actual.starts_with("#local:"),
        "the local declaration must actually win, got {actual}"
    );
}

#[test]
fn wildcard_imports_do_not_invent_certainty_against_implicit_java_lang() {
    let target = "package lab; public class String {}";
    let ambiguous = "package app; import lab.*; class Use { String value; }";
    let inventory = catalog(&[target, ambiguous]);
    let tree = parse_tree(ambiguous, Language::Java).unwrap();
    let bindings = JavaBindings::new(&tree, ambiguous);
    let reference = descendants(tree.root_node())
        .find(|node| node.kind() == "type_identifier" && text(*node, ambiguous) == "String")
        .unwrap();
    assert!(bindings.type_occurrence(reference, &inventory).is_err());
    for source in [
        "package app; import lab.String; import java.util.*; class Use { String value; }",
        "package lab; import java.util.*; class Use { String value; }",
    ] {
        let inventory = catalog(&[target, source]);
        let tree = parse_tree(source, Language::Java).unwrap();
        let bindings = JavaBindings::new(&tree, source);
        let reference = descendants(tree.root_node())
            .find(|node| node.kind() == "type_identifier" && text(*node, source) == "String")
            .unwrap();
        assert_eq!(
            bindings
                .type_occurrence(reference, &inventory)
                .unwrap()
                .as_deref(),
            Some("lab.String")
        );
        assert!(
            !bindings
                .type_name_capture(reference, "Text", &inventory)
                .unwrap(),
            "the explicit/same-package rename remains provable: {source}"
        );
    }
}

#[test]
fn ambiguous_type_imports_inheritance_and_patterns_refuse_real_occurrences() {
    for source in [
        "package app; import lab.*; import other.*; class Page { DocumentStore field; }",
        "package app; import lab.DocumentStore; import other.DocumentStore; class Page { DocumentStore field; }",
        "package lab; class Page extends UnknownBase { DocumentStore field; }",
        "package app; class DocumentStore extends UnknownBase { DocumentStore field; }",
        "package lab; class Page<DocumentStore> { class Child extends UnknownBase { DocumentStore field; } }",
        "package lab; class Page { static class DocumentStore {} class Child extends UnknownBase { DocumentStore field; } }",
        "package lab; class Page { int use(Object value) { if (value instanceof Page DocumentStore) { return DocumentStore.hashCode(); } return 0; } }",
    ] {
        let catalog = catalog(&[TARGET, OTHER, source]);
        let tree = parse_tree(source, Language::Java).unwrap();
        let bindings = JavaBindings::new(&tree, source);
        let occurrences = descendants(tree.root_node()).filter(|node| {
            matches!(node.kind(), "identifier" | "type_identifier") && text(*node, source) == "DocumentStore"
        }).collect::<Vec<_>>();
        assert!(!occurrences.is_empty(), "{source}");
        assert!(occurrences.into_iter().any(|node| bindings.type_occurrence(node, &catalog).is_err()), "{source}");
    }
}

#[test]
fn proposed_type_names_follow_declaration_grammar_and_restricted_identifiers() {
    for name in [
        "abstract",
        "assert",
        "boolean",
        "break",
        "byte",
        "case",
        "catch",
        "char",
        "class",
        "const",
        "continue",
        "default",
        "do",
        "double",
        "else",
        "enum",
        "extends",
        "final",
        "finally",
        "float",
        "for",
        "goto",
        "if",
        "implements",
        "import",
        "instanceof",
        "int",
        "interface",
        "long",
        "native",
        "new",
        "package",
        "private",
        "protected",
        "public",
        "return",
        "short",
        "static",
        "strictfp",
        "super",
        "switch",
        "synchronized",
        "this",
        "throw",
        "throws",
        "transient",
        "try",
        "void",
        "volatile",
        "while",
        "_",
        "null",
        "true",
        "false",
        "var",
        "yield",
        "record",
        "sealed",
        "permits",
        "bad-name",
    ] {
        assert!(!crate::ast::java::is_type_name(name), "{name}");
    }
    for name in [
        "DocumentLibrary",
        "module",
        "open",
        "with",
        "Library2",
        "$Library",
    ] {
        assert!(crate::ast::java::is_type_name(name), "{name}");
    }
}

#[test]
fn renamed_simple_types_refuse_lexical_import_and_value_receiver_capture() {
    for (source, expected) in [
        ("package lab; class Page<DocumentLibrary> { DocumentStore field; }", true),
        ("package app; import lab.DocumentStore; import other.DocumentLibrary; class Page { DocumentStore field; }", true),
        ("package lab; class Page { int use(int DocumentLibrary) { return DocumentStore.size(); } }", true),
        ("package lab; class Page { DocumentStore use(int DocumentLibrary) { return null; } }", false),
        ("package app; class Page<DocumentLibrary> { lab.DocumentStore field; }", false),
        ("package lab; class Page { DocumentStore field; }", false),
    ] {
        let catalog = catalog(&[TARGET, OTHER, source]);
        let tree = parse_tree(source, Language::Java).unwrap();
        let bindings = JavaBindings::new(&tree, source);
        let selected = descendants(tree.root_node()).filter(|node| {
            matches!(node.kind(), "identifier" | "type_identifier")
                && text(*node, source) == "DocumentStore"
                && bindings.type_occurrence(*node, &catalog).unwrap().as_deref() == Some("lab.DocumentStore")
        }).collect::<Vec<_>>();
        assert!(!selected.is_empty(), "the original type must actually bind: {source}");
        let capture = selected.into_iter().any(|node| bindings.type_name_capture(node, "DocumentLibrary", &catalog).unwrap());
        assert_eq!(capture, expected, "{source}");
    }
}
