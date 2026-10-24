//! A pattern's fix rewrites the whole matched node, so a match must not
//! carry target tokens the pattern neither spells nor binds: those tokens
//! would be deleted. These cases once matched and silently dropped a call
//! receiver, an argument, or a modifier, or doubled a `;`.

use harn_rules::{CompiledRule, Rule};

fn rewrite(language: &str, pattern: &str, fix: &str, source: &str) -> String {
    let rule = CompiledRule::compile(
        &Rule::from_toml_str(&format!(
            r#"
            id = "exact-shape"
            language = "{language}"
            safety = "behavior-preserving"
            fix = '{fix}'
            [rule]
            pattern = '{pattern}'
            "#
        ))
        .expect("parses"),
    )
    .expect("compiles");
    rule.apply(source).expect("applies").rewritten
}

const JAVA: &str = "class C {
    void f() {
        Folder folder = folderService.getById(folderId);
        Document document = getById(documentId);
        folderService.getById(otherId);
        getById(lastId);
    }
}
";

#[test]
fn java_statement_pattern_rewrites_only_bare_statements_and_keeps_one_semicolon() {
    let out = rewrite("java", "getById($A);", "requireDocument($A);", JAVA);
    assert_eq!(
        out,
        JAVA.replace(
            "        getById(lastId);",
            "        requireDocument(lastId);"
        ),
    );
}

#[test]
fn java_expression_pattern_without_semicolon_skips_receiver_calls() {
    let out = rewrite("java", "getById($A)", "requireDocument($A)", JAVA);
    assert_eq!(
        out,
        JAVA.replace(
            "document = getById(documentId);",
            "document = requireDocument(documentId);"
        )
        .replace(
            "        getById(lastId);",
            "        requireDocument(lastId);"
        ),
    );
}

#[test]
fn java_receiver_pattern_rewrites_only_that_receiver() {
    let source = "class C {
    void f() {
        Document a = documentService.getById(id);
        Folder b = folderService.getById(x);
    }
}
";
    let out = rewrite(
        "java",
        "documentService.getById($A)",
        "documentService.requireDocument($A)",
        source,
    );
    assert_eq!(
        out,
        source.replace(
            "documentService.getById(id);",
            "documentService.requireDocument(id);"
        ),
    );
}

#[test]
fn csharp_expression_pattern_without_semicolon_compiles_and_rewrites() {
    let source = "class C { void F() { var d = documents.GetById(id); } }\n";
    let out = rewrite(
        "csharp",
        "documents.GetById($A)",
        "documents.RequireDocument($A)",
        source,
    );
    assert_eq!(
        out,
        "class C { void F() { var d = documents.RequireDocument(id); } }\n"
    );
}

#[test]
fn ruby_receiverless_pattern_skips_receiver_calls() {
    // Ruby's `call` node carries the receiver in an optional field, the same
    // shape as Java's `method_invocation`.
    let source = "folder = folder_service.get_by_id(folder_id)\ndoc = get_by_id(doc_id)\n";
    let out = rewrite("ruby", "get_by_id($A)", "require_document($A)", source);
    assert_eq!(
        out,
        "folder = folder_service.get_by_id(folder_id)\ndoc = require_document(doc_id)\n"
    );
}

#[test]
fn single_argument_pattern_skips_calls_with_more_arguments() {
    let out = rewrite("typescript", "foo($A)", "bar($A)", "foo(a, b);\nfoo(c);\n");
    assert_eq!(out, "foo(a, b);\nbar(c);\n");
}

#[test]
fn pattern_without_modifier_skips_targets_with_one() {
    let source = "fn f() {\n    let x = 1;\n    let mut y = 2;\n}\n";
    let out = rewrite("rust", "let $N = $V;", "let $N: i32 = $V;", source);
    assert_eq!(
        out,
        "fn f() {\n    let x: i32 = 1;\n    let mut y = 2;\n}\n"
    );
}

#[test]
fn php_pattern_parses_as_php_not_inline_html() {
    let out = rewrite("php", "foo($A)", "bar($A)", "<?php\nfoo($x);\nbaz($y);\n");
    assert_eq!(out, "<?php\nbar($x);\nbaz($y);\n");
}

#[test]
fn pattern_without_keyword_skips_targets_with_one() {
    let source = "const f = (x) => x;\nconst g = async (y) => y;\n";
    let out = rewrite(
        "typescript",
        "($A) => $B",
        "function ($A) { return $B; }",
        source,
    );
    assert_eq!(
        out,
        "const f = function (x) { return x; };\nconst g = async (y) => y;\n"
    );
}

#[test]
fn metavars_named_like_internal_captures_still_match() {
    // The exact-shape and literal-text captures once shared the metavar
    // namespace, so these bound the wrong node and the pattern never matched.
    for metavar in ["__shape_0", "__shape_1", "__lit_0"] {
        let out = rewrite(
            "typescript",
            &format!("foo(${metavar})"),
            &format!("bar(${metavar})"),
            "foo(x);\n",
        );
        assert_eq!(out, "bar(x);\n", "${metavar}");
    }
}

#[test]
fn only_the_matched_statements_own_semicolon_may_be_omitted() {
    // The nested `foo();` keeps its `;` in the shape: matching it would let
    // the fix drop it and run the following statement into this one.
    let out = rewrite(
        "typescript",
        "if ($C) foo()",
        "if ($C) bar()",
        "if (c) foo(); bar();\nif (d) foo()\n",
    );
    assert_eq!(out, "if (c) foo(); bar();\nif (d) bar()\n");
}
