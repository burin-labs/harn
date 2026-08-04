//! End-to-end acceptance for #2834: `where` constraints, the `transform`
//! pipeline, and `fix` interpolation, applied as a codemod.

use harn_rules::{CompiledRule, Rule};

fn compile(toml: &str) -> CompiledRule {
    CompiledRule::compile(&Rule::from_toml_str(toml).expect("rule parses")).expect("rule compiles")
}

#[test]
fn destructuring_fix_handles_alias_and_shorthand() {
    // The #2824 customer's single-binding fold + alias handling:
    //   `let id = src?.userId`     -> `{ userId: id }`     (aliased)
    //   `let userId = src?.userId` -> `{ userId: userId }` (shorthand once
    //                                  `harn fmt` collapses the duplicate key)
    let rule = compile(
        r#"
        id = "fold-optional-bind"
        language = "typescript"
        fix = "{ $KEY: $NAME }"
        [rule]
        pattern = "let $NAME = $SRC?.$KEY"
        "#,
    );

    let aliased = rule.apply("let id = src?.userId;\n").unwrap();
    assert!(aliased.changed);
    assert!(
        aliased.rewritten.contains("{ userId: id }"),
        "got: {}",
        aliased.rewritten
    );

    let shorthand = rule.apply("let userId = src?.userId;\n").unwrap();
    assert!(
        shorthand.rewritten.contains("{ userId: userId }"),
        "got: {}",
        shorthand.rewritten
    );
}

#[test]
fn fix_uses_a_transform_synthesized_metavar() {
    // Rename a camelCase call to snake_case using a `convert` transform,
    // then interpolate the synthesized metavar into the fix.
    let rule = compile(
        r#"
        id = "snakeify-call"
        language = "typescript"
        fix = "$SNAKE()"
        [rule]
        pattern = "$FN()"
        [transform.SNAKE]
        source = "FN"
        convert = "snake"
        "#,
    );
    let result = rule.apply("getUserId();\n").unwrap();
    assert!(
        result.rewritten.contains("get_user_id()"),
        "got: {}",
        result.rewritten
    );
}

#[test]
fn where_constraint_filters_matches() {
    // Only rewrite calls whose callee matches the constraint regex.
    let rule = compile(
        r#"
        id = "guarded-rename"
        language = "typescript"
        fix = "renamed()"
        [rule]
        pattern = "$FN()"
        [[where]]
        metavar = "FN"
        regex = "^legacy"
        "#,
    );
    let result = rule
        .apply("legacyInit();\nkeepThis();\nlegacyTeardown();\n")
        .unwrap();
    // The two `legacy*` calls are rewritten; `keepThis()` is untouched.
    assert_eq!(result.edits.len(), 2);
    assert!(result.rewritten.contains("keepThis()"));
    assert_eq!(result.rewritten.matches("renamed()").count(), 2);
}

#[test]
fn comparison_constraint_filters_numerically() {
    // Only flag default values greater than 100.
    let rule = compile(
        r#"
        id = "big-default"
        language = "typescript"
        fix = "CAPPED"
        [rule]
        pattern = "$SRC?.$KEY ?? $N"
        [[where]]
        metavar = "N"
        comparison = { op = ">", value = 100 }
        "#,
    );
    let result = rule
        .apply("const a = o?.x ?? 5;\nconst b = o?.y ?? 500;\n")
        .unwrap();
    assert_eq!(result.edits.len(), 1);
    assert!(result.rewritten.contains("?? 5"));
    assert!(result.rewritten.contains("CAPPED"));
}

#[test]
fn apply_without_fix_errors() {
    let rule = compile(
        r#"
        id = "search-only"
        language = "typescript"
        [rule]
        pattern = "$FN()"
        "#,
    );
    assert!(rule.apply("foo();").is_err());
}
