use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;

use super::*;

fn dict(pairs: &[(&str, VmValue)]) -> BTreeMap<String, VmValue> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}

fn s(v: &str) -> VmValue {
    VmValue::String(Rc::from(v))
}

fn render(tpl: &str, b: &BTreeMap<String, VmValue>) -> String {
    render_template_result(tpl, Some(b), None, None).unwrap()
}

fn render_with_spans(tpl: &str, b: &BTreeMap<String, VmValue>) -> (String, Vec<PromptSourceSpan>) {
    render_template_with_provenance(tpl, Some(b), None, None, true).unwrap()
}

#[test]
fn bare_interp() {
    let b = dict(&[("name", s("Alice"))]);
    assert_eq!(render("hi {{name}}!", &b), "hi Alice!");
}

#[test]
fn provenance_expr_span_matches_output_range() {
    let mut user = BTreeMap::new();
    user.insert("name".to_string(), s("alice"));
    let b = dict(&[
        ("user", VmValue::Dict(Rc::new(user))),
        ("count", VmValue::Int(42)),
    ]);
    let (out, spans) = render_with_spans("hello {{ user.name }} ({{ count | default: 0 }})", &b);
    assert_eq!(out, "hello alice (42)");

    let expr_spans: Vec<_> = spans
        .iter()
        .filter(|s| s.kind == PromptSpanKind::Expr)
        .collect();
    assert_eq!(expr_spans.len(), 2);

    let user_span = expr_spans
        .iter()
        .find(|s| &out[s.output_start..s.output_end] == "alice")
        .expect("user expr span");
    assert!(user_span.template_line >= 1);
    assert_eq!(user_span.bound_value.as_deref(), Some("alice"));

    let count_span = expr_spans
        .iter()
        .find(|s| &out[s.output_start..s.output_end] == "42")
        .expect("count expr span");
    assert_eq!(count_span.bound_value.as_deref(), Some("42"));
}

#[test]
fn provenance_legacy_bare_interp_span_tracked() {
    let b = dict(&[("name", s("Alice"))]);
    let (out, spans) = render_with_spans("hi {{name}}!", &b);
    assert_eq!(out, "hi Alice!");

    let bare = spans
        .iter()
        .find(|s| s.kind == PromptSpanKind::LegacyBareInterp)
        .expect("legacy bare span");
    assert_eq!(&out[bare.output_start..bare.output_end], "Alice");
    assert_eq!(bare.bound_value.as_deref(), Some("Alice"));
}

#[test]
fn provenance_includes_loop_iterations() {
    let b = dict(&[(
        "items",
        VmValue::List(Rc::new(vec![s("a"), s("b"), s("c")])),
    )]);
    let tpl = "{{for x in items}}[{{x}}]{{end}}";
    let (out, spans) = render_with_spans(tpl, &b);
    assert_eq!(out, "[a][b][c]");
    let iter_spans: Vec<_> = spans
        .iter()
        .filter(|s| s.kind == PromptSpanKind::ForIteration)
        .collect();
    assert_eq!(iter_spans.len(), 3);
    let slices: Vec<&str> = iter_spans
        .iter()
        .map(|s| &out[s.output_start..s.output_end])
        .collect();
    assert_eq!(slices, ["[a]", "[b]", "[c]"]);
}

#[test]
fn provenance_preview_is_truncated() {
    let mut wrap = BTreeMap::new();
    wrap.insert("val".to_string(), s(&"x".repeat(500)));
    let b = dict(&[("blob", VmValue::Dict(Rc::new(wrap)))]);
    let (_, spans) = render_with_spans("{{blob.val}}", &b);
    let expr = spans
        .iter()
        .find(|s| s.kind == PromptSpanKind::Expr)
        .expect("expr span");
    let preview = expr.bound_value.as_deref().unwrap();
    assert!(preview.chars().count() <= 80, "preview too long: {preview}");
    assert!(preview.ends_with('…'));
}

#[test]
fn provenance_off_returns_empty_spans() {
    let b = dict(&[("x", s("y"))]);
    let (_, spans) = render_template_with_provenance("{{x}}", Some(&b), None, None, false).unwrap();
    assert!(spans.is_empty());
}

#[test]
fn bare_interp_missing_passthrough() {
    let b = dict(&[]);
    assert_eq!(render("hi {{name}}!", &b), "hi {{name}}!");
}

#[test]
fn legacy_if_truthy() {
    let b = dict(&[("x", VmValue::Bool(true))]);
    assert_eq!(render("{{if x}}yes{{end}}", &b), "yes");
}

#[test]
fn legacy_if_falsey() {
    let b = dict(&[("x", VmValue::Bool(false))]);
    assert_eq!(render("{{if x}}yes{{end}}", &b), "");
}

#[test]
fn if_else() {
    let b = dict(&[("x", VmValue::Bool(false))]);
    assert_eq!(render("{{if x}}A{{else}}B{{end}}", &b), "B");
}

#[test]
fn if_elif_else() {
    let b = dict(&[("n", VmValue::Int(2))]);
    let tpl = "{{if n == 1}}one{{elif n == 2}}two{{elif n == 3}}three{{else}}many{{end}}";
    assert_eq!(render(tpl, &b), "two");
}

#[test]
fn for_loop_basic() {
    let items = VmValue::List(Rc::new(vec![s("a"), s("b"), s("c")]));
    let b = dict(&[("xs", items)]);
    assert_eq!(render("{{for x in xs}}{{x}},{{end}}", &b), "a,b,c,");
}

#[test]
fn for_loop_vars() {
    let items = VmValue::List(Rc::new(vec![s("a"), s("b")]));
    let b = dict(&[("xs", items)]);
    let tpl = "{{for x in xs}}{{loop.index}}:{{x}}{{if !loop.last}},{{end}}{{end}}";
    assert_eq!(render(tpl, &b), "1:a,2:b");
}

#[test]
fn for_empty_else() {
    let b = dict(&[("xs", VmValue::List(Rc::new(vec![])))]);
    assert_eq!(render("{{for x in xs}}A{{else}}empty{{end}}", &b), "empty");
}

#[test]
fn for_dict_kv() {
    let mut d: BTreeMap<String, VmValue> = BTreeMap::new();
    d.insert("a".into(), VmValue::Int(1));
    d.insert("b".into(), VmValue::Int(2));
    let b = dict(&[("m", VmValue::Dict(Rc::new(d)))]);
    assert_eq!(
        render("{{for k, v in m}}{{k}}={{v}};{{end}}", &b),
        "a=1;b=2;"
    );
}

#[test]
fn nested_path() {
    let mut inner: BTreeMap<String, VmValue> = BTreeMap::new();
    inner.insert("name".into(), s("Alice"));
    let b = dict(&[("user", VmValue::Dict(Rc::new(inner)))]);
    assert_eq!(render("{{user.name}}", &b), "Alice");
}

#[test]
fn list_index() {
    let b = dict(&[("xs", VmValue::List(Rc::new(vec![s("a"), s("b"), s("c")])))]);
    assert_eq!(render("{{xs[1]}}", &b), "b");
}

#[test]
fn filter_upper() {
    let b = dict(&[("n", s("alice"))]);
    assert_eq!(render("{{n | upper}}", &b), "ALICE");
}

#[test]
fn filter_default() {
    let b = dict(&[("n", s(""))]);
    assert_eq!(render("{{n | default: \"anon\"}}", &b), "anon");
}

#[test]
fn filter_join() {
    let b = dict(&[("xs", VmValue::List(Rc::new(vec![s("a"), s("b")])))]);
    assert_eq!(render("{{xs | join: \", \"}}", &b), "a, b");
}

#[test]
fn comparison_ops() {
    let b = dict(&[("n", VmValue::Int(5))]);
    assert_eq!(render("{{if n > 3}}big{{end}}", &b), "big");
    assert_eq!(render("{{if n >= 5 and n < 10}}ok{{end}}", &b), "ok");
}

#[test]
fn bool_not() {
    let b = dict(&[("x", VmValue::Bool(false))]);
    assert_eq!(render("{{if not x}}yes{{end}}", &b), "yes");
    assert_eq!(render("{{if !x}}yes{{end}}", &b), "yes");
}

#[test]
fn raw_block() {
    let b = dict(&[]);
    assert_eq!(
        render("A {{ raw }}{{not-a-directive}}{{ endraw }} B", &b),
        "A {{not-a-directive}} B"
    );
}

#[test]
fn comment_stripped() {
    let b = dict(&[("x", s("hi"))]);
    assert_eq!(render("A{{# hidden #}}B{{x}}", &b), "ABhi");
}

#[test]
fn whitespace_trim() {
    let b = dict(&[("x", s("v"))]);
    let tpl = "line1\n  {{- x -}}  \nline2";
    assert_eq!(render(tpl, &b), "line1vline2");
}

#[test]
fn filter_json() {
    let b = dict(&[(
        "x",
        VmValue::Dict(Rc::new({
            let mut m = BTreeMap::new();
            m.insert("a".into(), VmValue::Int(1));
            m
        })),
    )]);
    assert_eq!(render("{{x | json}}", &b), r#"{"a":1}"#);
}

#[test]
fn error_unterminated_if() {
    let b = dict(&[("x", VmValue::Bool(true))]);
    let r = render_template_result("{{if x}}open", Some(&b), None, None);
    assert!(r.is_err());
}

#[test]
fn error_unknown_filter() {
    let b = dict(&[("x", s("a"))]);
    let r = render_template_result("{{x | bogus}}", Some(&b), None, None);
    assert!(r.is_err());
}

#[test]
fn include_with() {
    use std::fs;
    let dir = tempdir();
    let partial = dir.join("p.prompt");
    fs::write(&partial, "[{{name}}]").unwrap();
    let parent = dir.join("main.prompt");
    fs::write(
        &parent,
        r#"hello {{ include "p.prompt" with { name: who } }}!"#,
    )
    .unwrap();
    let b = dict(&[("who", s("world"))]);
    let src = fs::read_to_string(&parent).unwrap();
    let out = render_template_result(&src, Some(&b), Some(&dir), Some(&parent)).unwrap();
    assert_eq!(out, "hello [world]!");
}

#[test]
fn prompt_render_indices_accumulate_in_order() {
    reset_prompt_registry();
    record_prompt_render_index("p-1", 5);
    record_prompt_render_index("p-1", 9);
    record_prompt_render_index("p-2", 7);
    let p1 = prompt_render_indices("p-1");
    assert_eq!(p1, vec![5, 9]);
    let p2 = prompt_render_indices("p-2");
    assert_eq!(p2, vec![7]);
    assert!(prompt_render_indices("unknown").is_empty());
    reset_prompt_registry();
    assert!(
        prompt_render_indices("p-1").is_empty(),
        "reset clears the map"
    );
}

#[test]
fn include_propagates_parent_span_chain() {
    use std::fs;
    let dir = tempdir();
    let leaf = dir.join("leaf.prompt");
    fs::write(&leaf, "LEAF:{{v}}").unwrap();
    let mid = dir.join("mid.prompt");
    fs::write(&mid, r#"MID:{{ include "leaf.prompt" }}"#).unwrap();
    let top = dir.join("top.prompt");
    fs::write(&top, r#"TOP:{{ include "mid.prompt" }}"#).unwrap();
    let b = dict(&[("v", s("ok"))]);
    let src = fs::read_to_string(&top).unwrap();
    let (rendered, spans) =
        render_template_with_provenance(&src, Some(&b), Some(&dir), Some(&top), true).unwrap();
    assert_eq!(rendered, "TOP:MID:LEAF:ok");

    let leaf_expr = spans
        .iter()
        .find(|s| {
            matches!(
                s.kind,
                PromptSpanKind::Expr | PromptSpanKind::LegacyBareInterp
            ) && s.parent_span.is_some()
        })
        .expect("interpolation span emitted");
    let mid_parent = leaf_expr
        .parent_span
        .as_deref()
        .expect("leaf span must have mid's include as parent");
    assert_eq!(mid_parent.kind, PromptSpanKind::Include);
    let top_parent = mid_parent
        .parent_span
        .as_deref()
        .expect("mid's include must chain up to top's include");
    assert_eq!(top_parent.kind, PromptSpanKind::Include);
    assert!(top_parent.parent_span.is_none(), "chain bottoms out at top");

    assert!(leaf_expr.template_uri.ends_with("leaf.prompt"));
    assert!(mid_parent.template_uri.ends_with("mid.prompt"));
    assert!(top_parent.template_uri.ends_with("top.prompt"));
}

#[test]
fn include_cycle_detected() {
    use std::fs;
    let dir = tempdir();
    let a = dir.join("a.prompt");
    let b = dir.join("b.prompt");
    fs::write(&a, r#"A{{ include "b.prompt" }}"#).unwrap();
    fs::write(&b, r#"B{{ include "a.prompt" }}"#).unwrap();
    let src = fs::read_to_string(&a).unwrap();
    let r = render_template_result(&src, None, Some(&dir), Some(&a));
    assert!(r.is_err());
    assert!(r.unwrap_err().kind.contains("circular include"));
}

fn tempdir() -> PathBuf {
    let base = std::env::temp_dir().join(format!("harn-tpl-{}", nanoid()));
    std::fs::create_dir_all(&base).unwrap();
    base
}

fn nanoid() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    format!(
        "{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )
}
