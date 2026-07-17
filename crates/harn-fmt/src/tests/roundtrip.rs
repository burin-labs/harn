use super::assert_roundtrip;
use crate::format_source;

#[test]
fn basic_constructs_round_trip() {
    for source in [
        "pipeline default(task) { let x = 42\nlog(x) }",
        "pipeline default(task) { fn add(a, b) { return a + b }\nlog(add(1, 2)) }",
        "pipeline default(task) { fn id<T>(x: T) -> T { return x }\nlet x = id<int>(1) }",
        "pipeline default(task) { let f = { x -> x * 2 }\nlog(f(3)) }",
        "pipeline default(task) { if true { log(1) } else { log(2) } }",
        r#"pipeline default(task) { try { throw "e" } catch (e) { log(e) } }"#,
        r#"pipeline default(task) { try { throw "e" } catch {} }"#,
        r#"pipeline default(task) { try { throw "e" } catch {} finally { log("done") } }"#,
        "pipeline default(task) { for i in [1, 2, 3] { log(i) } }",
        r#"pipeline default(task) { match x { "a" -> { log(1) } "b" -> { log(2) } } }"#,
        "interface Printable {\n  fn to_display() -> string\n}\npipeline default(task) { log(1) }",
        "pub pipeline build(task) extends base {\n  return\n}\n\npub enum Result {\n  Ok(value: string)\n}\n\npub struct Config {\n  port?: int\n}\n\npub type ConfigAlias = {port: int}\n\ninterface Repository<T> {\n  fn map<U>(value: T, f: fn(T) -> U) -> U\n}",
        "enum Color {\n  Red\n  Green\n  Blue\n}\npipeline default(task) { log(1) }",
    ] {
        assert_roundtrip(source);
    }
}

#[test]
fn range_subexpressions_keep_required_parentheses() {
    assert_roundtrip("pipeline default(task) { let x = c ? (a to b) : d }");
    assert_roundtrip("pipeline default(task) { let x = (a to b) to c }");

    for source in [
        "pipeline default(task) { let x = (a to b) ? c : d }",
        "pipeline default(task) { let x = c ? d : (a to b) }",
        "pipeline default(task) { let x = (a to b) < c }",
    ] {
        let formatted = format_source(source).unwrap();
        assert!(
            formatted.contains("(a to b)"),
            "range subexpression must stay parenthesized:\n{formatted}"
        );
        assert_roundtrip(source);
    }
}

#[test]
fn zero_argument_closures_keep_their_arrow() {
    for source in [
        "pipeline default(task) { let f = { -> 1 }\nlog(f()) }",
        "pipeline default(task) { fn compute() { return 1 }\nlet f = { -> compute() }\nlog(f()) }",
        "pipeline default(task) { fn heavy() { return 1 }\nfn with_cache(k, f, opts) { return f() }\nlet c = with_cache(\"k\", { -> heavy() }, {}) }",
        "pipeline default(task) {\n  let f = { ->\n    let x = 1\n    x + 1\n  }\n  log(f())\n}",
    ] {
        let formatted = format_source(source).unwrap();
        assert!(
            formatted.contains("{ ->"),
            "zero-argument closure lost its arrow:\n{formatted}"
        );
        assert_roundtrip(source);
    }
}

#[test]
fn discard_bindings_round_trip() {
    let source = "pipeline default(task) {\n  let _ = 1\n  let _ = 2\n  let [_, keep, _] = [10, 20, 30]\n  __io_println(keep)\n}";
    let formatted = format_source(source).unwrap();
    assert!(formatted.contains("let _ = 1\n"));
    assert!(formatted.contains("let _ = 2\n"));
    assert!(formatted.contains("let [_, keep, _] = [10, 20, 30]\n"));
    assert_roundtrip(source);
}

#[test]
fn match_expressions_and_dict_keys_round_trip() {
    let match_source = r#"pipeline default(task) { let label = match x { "a" -> { "alpha" } "b" if keep -> { "bravo" } _ -> { "other" } } }"#;
    let formatted_match = format_source(match_source).unwrap();
    assert!(formatted_match.contains("let label = match x {\n"));
    assert!(formatted_match.contains(r#""b" if keep -> { "bravo" }"#));
    assert_roundtrip(match_source);

    assert_roundtrip(
        r#"pipeline default(task) { let k = "x"
  let d = {[k]: 42, fixed: 1} }"#,
    );
    let dict_source = r#"pipeline default(task) {
  let d = {["a.b.c"]: "x", k: "y", ["with space"]: 1}
}"#;
    let formatted_dict = format_source(dict_source).unwrap();
    assert!(formatted_dict.contains(r#"{"a.b.c": "x", k: "y", "with space": 1}"#));
    assert_roundtrip(dict_source);
}
