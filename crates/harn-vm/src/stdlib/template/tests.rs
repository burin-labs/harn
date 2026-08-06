use super::*;
use crate::value::VmValue;

fn dict(pairs: &[(&str, VmValue)]) -> crate::value::DictMap {
    pairs
        .iter()
        .map(|(k, v)| (crate::value::intern_key(k), v.clone()))
        .collect()
}

fn s(v: &str) -> VmValue {
    VmValue::String(arcstr::ArcStr::from(v))
}

fn render(tpl: &str, b: &crate::value::DictMap) -> String {
    render_template_result(tpl, Some(b), None, None).unwrap()
}

fn render_with_spans(tpl: &str, b: &crate::value::DictMap) -> (String, Vec<PromptSourceSpan>) {
    render_template_with_provenance(tpl, Some(b), None, None, true).unwrap()
}

fn list(items: Vec<VmValue>) -> VmValue {
    VmValue::List(std::sync::Arc::new(items))
}

#[test]
fn bare_interp() {
    let b = dict(&[("name", s("Alice"))]);
    assert_eq!(render("hi {{name}}!", &b), "hi Alice!");
}

#[test]
fn provenance_expr_span_matches_output_range() {
    let mut user = crate::value::DictMap::new();
    user.insert(crate::value::intern_key("name"), s("alice"));
    let b = dict(&[("user", VmValue::dict(user)), ("count", VmValue::Int(42))]);
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
        VmValue::List(std::sync::Arc::new(vec![s("a"), s("b"), s("c")])),
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
    let mut wrap = crate::value::DictMap::new();
    wrap.insert(crate::value::intern_key("val"), s(&"x".repeat(500)));
    let b = dict(&[("blob", VmValue::dict(wrap))]);
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
    let items = VmValue::List(std::sync::Arc::new(vec![s("a"), s("b"), s("c")]));
    let b = dict(&[("xs", items)]);
    assert_eq!(render("{{for x in xs}}{{x}},{{end}}", &b), "a,b,c,");
}

#[test]
fn for_loop_vars() {
    let items = VmValue::List(std::sync::Arc::new(vec![s("a"), s("b")]));
    let b = dict(&[("xs", items)]);
    let tpl = "{{for x in xs}}{{loop.index}}:{{x}}{{if !loop.last}},{{end}}{{end}}";
    assert_eq!(render(tpl, &b), "1:a,2:b");
}

#[test]
fn for_empty_else() {
    let b = dict(&[("xs", VmValue::List(std::sync::Arc::new(vec![])))]);
    assert_eq!(render("{{for x in xs}}A{{else}}empty{{end}}", &b), "empty");
}

#[test]
fn for_dict_kv() {
    let mut d: crate::value::DictMap = crate::value::DictMap::new();
    d.insert("a".into(), VmValue::Int(1));
    d.insert("b".into(), VmValue::Int(2));
    let b = dict(&[("m", VmValue::dict(d))]);
    assert_eq!(
        render("{{for k, v in m}}{{k}}={{v}};{{end}}", &b),
        "a=1;b=2;"
    );
}

#[test]
fn nested_path() {
    let mut inner: crate::value::DictMap = crate::value::DictMap::new();
    inner.insert("name".into(), s("Alice"));
    let b = dict(&[("user", VmValue::dict(inner))]);
    assert_eq!(render("{{user.name}}", &b), "Alice");
}

#[test]
fn list_index() {
    let b = dict(&[(
        "xs",
        VmValue::List(std::sync::Arc::new(vec![s("a"), s("b"), s("c")])),
    )]);
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
    let b = dict(&[(
        "xs",
        VmValue::List(std::sync::Arc::new(vec![s("a"), s("b")])),
    )]);
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
fn logical_section_scaffolding_follows_llm_capabilities() {
    let b = dict(&[]);
    let tpl = "{{ section \"task\" }}Build a tree CLI.{{ endsection }}";

    {
        let _guard =
            LlmRenderContextGuard::enter(LlmRenderContext::resolve("anthropic", "claude-opus-4-7"));
        assert_eq!(render(tpl, &b), "<task>\nBuild a tree CLI.\n</task>");
    }

    {
        let _guard = LlmRenderContextGuard::enter(LlmRenderContext::resolve("openai", "gpt-5.4"));
        assert_eq!(render(tpl, &b), "## Task\nBuild a tree CLI.");
    }

    assert_eq!(render(tpl, &b), "Task:\nBuild a tree CLI.");
}

#[test]
fn logical_section_tools_and_output_format_use_section_args() {
    let tool = VmValue::dict(crate::value::DictMap::from_iter([
        (crate::value::intern_key("name"), s("read_file")),
        (crate::value::intern_key("description"), s("Read a file")),
    ]));
    let schema = VmValue::dict(crate::value::DictMap::from_iter([
        (crate::value::intern_key("type"), s("object")),
        (
            crate::value::intern_key("properties"),
            VmValue::dict(crate::value::DictMap::from_iter([(
                crate::value::intern_key("answer"),
                s("string"),
            )])),
        ),
    ]));
    let b = dict(&[("tools", list(vec![tool])), ("schema", schema)]);

    {
        let _guard =
            LlmRenderContextGuard::enter(LlmRenderContext::resolve("anthropic", "claude-opus-4-7"));
        let out = render(
            "{{ section \"tools\" tools=tools }}{{ endsection }}\n{{ section \"output_format\" schema=schema }}{{ endsection }}",
            &b,
        );
        assert!(out.contains("<tools>"));
        assert!(out.contains("\"name\": \"read_file\""));
        assert!(out.contains("<output_format>"));
        assert!(out.contains("\"answer\": \"string\""));
    }

    {
        let _guard = LlmRenderContextGuard::enter(LlmRenderContext::resolve("openai", "gpt-5.4"));
        let out = render(
            "{{ section \"output_format\" schema=schema }}{{ endsection }}",
            &b,
        );
        assert_eq!(out, "");
    }

    {
        let _guard = LlmRenderContextGuard::enter(LlmRenderContext::resolve(
            "ollama",
            "devstral-small-2:24b",
        ));
        let out = render("{{ section \"tools\" tools=tools }}{{ endsection }}", &b);
        assert!(out.contains("ReAct-style envelope"));
        assert!(out.contains("Action Input: <json arguments>"));
        assert!(out.contains("\"name\": \"read_file\""));
    }
}

#[test]
fn logical_section_errors_and_legacy_bare_compat() {
    let b = dict(&[]);
    assert_eq!(render("{{ section_name }}", &b), "{{section_name}}");

    let missing_name =
        render_template_result("{{ section }}x{{ endsection }}", Some(&b), None, None);
    assert!(missing_name
        .unwrap_err()
        .kind
        .contains("expected section name"));

    let unknown = render_template_result(
        "{{ section \"bogus\" }}x{{ endsection }}",
        Some(&b),
        None,
        None,
    );
    assert!(unknown
        .unwrap_err()
        .kind
        .contains("unknown template section"));

    let unclosed = render_template_result("{{ section \"task\" }}x", Some(&b), None, None);
    assert!(unclosed.unwrap_err().kind.contains("missing matching"));

    let mismatched = render_template_result(
        "{{ section \"task\" }}x{{ endsection \"examples\" }}",
        Some(&b),
        None,
        None,
    );
    assert!(mismatched
        .unwrap_err()
        .kind
        .contains("mismatched section end"));
}

#[test]
fn logical_section_nests_inside_if_for_and_whitespace_trim() {
    let b = dict(&[("show", VmValue::Bool(true))]);
    let out = render(
        "{{- if show -}}{{ section \"task\" }}T{{ endsection }}{{- end -}}",
        &b,
    );
    assert_eq!(out, "Task:\nT");

    let names = list(vec![s("Alice"), s("Bob")]);
    let b = dict(&[("names", names)]);
    let out = render(
        "{{ for n in names }}{{ section \"task\" }}For {{ n }}{{ endsection }}\n{{ end }}",
        &b,
    );
    assert_eq!(out, "Task:\nFor Alice\nTask:\nFor Bob\n");
}

#[test]
fn logical_section_provenance_adjusts_child_spans() {
    let b = dict(&[("name", s("Alice"))]);
    let (out, spans) = render_with_spans(
        "{{ section \"task\" }}Hi {{ name | upper }}{{ endsection }}",
        &b,
    );
    assert_eq!(out, "Task:\nHi ALICE");
    assert!(spans.iter().any(|s| s.kind == PromptSpanKind::Section));
    let expr = spans
        .iter()
        .find(|s| s.kind == PromptSpanKind::Expr)
        .expect("expr span");
    assert_eq!(&out[expr.output_start..expr.output_end], "ALICE");

    let (out, spans) = render_with_spans(
        "{{ section \"tools\" }}Tool owner: {{ name | upper }}{{ endsection }}",
        &b,
    );
    assert_eq!(out, "Tools:\nTool owner: ALICE");
    let expr = spans
        .iter()
        .find(|s| s.kind == PromptSpanKind::Expr)
        .expect("tools expr span");
    assert_eq!(&out[expr.output_start..expr.output_end], "ALICE");
}

#[test]
fn filter_json() {
    let b = dict(&[(
        "x",
        VmValue::dict({
            let mut m = crate::value::DictMap::new();
            m.insert("a".into(), VmValue::Int(1));
            m
        }),
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
    let partial = dir.path().join("p.prompt");
    fs::write(&partial, "[{{name}}]").unwrap();
    let parent = dir.path().join("main.prompt");
    fs::write(
        &parent,
        r#"hello {{ include "p.prompt" with { name: who } }}!"#,
    )
    .unwrap();
    let b = dict(&[("who", s("world"))]);
    let src = fs::read_to_string(&parent).unwrap();
    let out = render_template_result(&src, Some(&b), Some(dir.path()), Some(&parent)).unwrap();
    assert_eq!(out, "hello [world]!");
}

#[test]
fn stdlib_prompt_asset_renders_and_includes_embedded_partial() {
    let asset =
        TemplateAsset::render_target("std/agent/prompts/tool_contract_text.harn.prompt").unwrap();
    let out = render_asset_result(&asset, Some(&dict(&[]))).unwrap();
    assert!(out.contains("## Tool Calling Contract"));
    assert!(out.contains("## Available tools"));
}

#[test]
fn stdlib_prompt_provenance_uses_stable_template_uris() {
    let asset =
        TemplateAsset::render_target("std/agent/prompts/tool_contract_text.harn.prompt").unwrap();
    let bindings = dict(&[
        ("mode", s("text")),
        ("text_response_protocol", s("rendered response protocol")),
        ("expanded_schemas", s("rendered schemas")),
    ]);
    let (out, spans) = render_asset_with_provenance_result(&asset, Some(&bindings), true).unwrap();
    assert!(out.contains("## Tool Calling Contract"));
    assert!(out.contains("rendered response protocol"));
    assert!(spans
        .iter()
        .any(|span| span.template_uri == "std://agent/prompts/tool_contract_text.harn.prompt"));
    assert!(spans
        .iter()
        .any(|span| span.bound_value.as_deref() == Some("rendered response protocol")));
}

#[test]
fn stdlib_template_cache_reuses_parsed_asset() {
    super::assets::reset_template_cache();
    let asset = TemplateAsset::render_target("std/workflow/prompts/stage.harn.prompt").unwrap();
    let bindings = dict(&[("name", s("triage")), ("goal", s("Sort the queue."))]);
    let first = render_asset_result(&asset, Some(&bindings)).unwrap();
    let count_after_first = super::assets::template_cache_len();
    let second = render_asset_result(&asset, Some(&bindings)).unwrap();
    assert_eq!(first, second);
    assert_eq!(count_after_first, super::assets::template_cache_len());
}

#[test]
fn filesystem_template_cache_invalidates_when_contents_change() {
    use std::fs;
    super::assets::reset_template_cache();
    let dir = tempdir();
    let path = dir.path().join("main.prompt");
    fs::write(&path, "one {{x}}").unwrap();
    let first = TemplateAsset::render_target(path.to_str().unwrap()).unwrap();
    assert_eq!(
        render_asset_result(&first, Some(&dict(&[("x", s("render"))]))).unwrap(),
        "one render"
    );
    fs::write(&path, "two {{x}}").unwrap();
    let second = TemplateAsset::render_target(path.to_str().unwrap()).unwrap();
    assert_eq!(
        render_asset_result(&second, Some(&dict(&[("x", s("render"))]))).unwrap(),
        "two render"
    );
    assert_eq!(super::assets::template_cache_len(), 2);
}

#[test]
fn package_root_include_still_resolves() {
    use std::fs;
    let dir = tempdir();
    fs::write(dir.path().join("harn.toml"), "[package]\nname = \"x\"\n").unwrap();
    fs::create_dir_all(dir.path().join("prompts")).unwrap();
    fs::write(
        dir.path().join("prompts/partial.harn.prompt"),
        "ROOT:{{name}}",
    )
    .unwrap();
    let parent = dir.path().join("main.prompt");
    fs::write(&parent, r#"{{ include "@/prompts/partial.harn.prompt" }}"#).unwrap();
    let src = fs::read_to_string(&parent).unwrap();
    let out = render_template_result(
        &src,
        Some(&dict(&[("name", s("ok"))])),
        Some(dir.path()),
        Some(&parent),
    )
    .unwrap();
    assert_eq!(out, "ROOT:ok");
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
    let leaf = dir.path().join("leaf.prompt");
    fs::write(&leaf, "LEAF:{{v}}").unwrap();
    let mid = dir.path().join("mid.prompt");
    fs::write(&mid, r#"MID:{{ include "leaf.prompt" }}"#).unwrap();
    let top = dir.path().join("top.prompt");
    fs::write(&top, r#"TOP:{{ include "mid.prompt" }}"#).unwrap();
    let b = dict(&[("v", s("ok"))]);
    let src = fs::read_to_string(&top).unwrap();
    let (rendered, spans) =
        render_template_with_provenance(&src, Some(&b), Some(dir.path()), Some(&top), true)
            .unwrap();
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
    let a = dir.path().join("a.prompt");
    let b = dir.path().join("b.prompt");
    fs::write(&a, r#"A{{ include "b.prompt" }}"#).unwrap();
    fs::write(&b, r#"B{{ include "a.prompt" }}"#).unwrap();
    let src = fs::read_to_string(&a).unwrap();
    let r = render_template_result(&src, None, Some(dir.path()), Some(&a));
    assert!(r.is_err());
    let kind = r.unwrap_err().kind;
    assert!(kind.contains("circular include"));
    assert!(kind.contains("a.prompt"), "{kind}");
    assert!(kind.contains("b.prompt"), "{kind}");
    assert!(kind.contains('→'), "{kind}");
}

#[test]
fn include_depth_limit_reports_runtime_ceiling() {
    use crate::runtime_limits::RuntimeLimits;
    use std::fs;

    let dir = tempdir();
    let limit = RuntimeLimits::DEFAULT.max_template_include_depth;
    for index in 0..=limit + 1 {
        let path = dir.path().join(format!("{index}.prompt"));
        let body = if index == limit + 1 {
            "leaf".to_string()
        } else {
            format!(r#"{{{{ include "{}.prompt" }}}}"#, index + 1)
        };
        fs::write(path, body).unwrap();
    }

    let parent = dir.path().join("0.prompt");
    let src = fs::read_to_string(&parent).unwrap();
    let err = render_template_result(&src, None, Some(dir.path()), Some(&parent))
        .expect_err("include chain should exceed the template include ceiling");
    assert!(
        err.kind
            .contains(&format!("include depth exceeded ({limit} levels)")),
        "{}",
        err.kind
    );
}

#[test]
fn include_cannot_escape_template_root() {
    use std::fs;
    let dir = tempdir();
    let sibling_name = format!(
        "{}-outside",
        dir.path().file_name().unwrap().to_string_lossy()
    );
    let outside_dir = dir.path().parent().unwrap().join(&sibling_name);
    fs::create_dir_all(&outside_dir).unwrap();
    fs::write(outside_dir.join("secret.prompt"), "secret").unwrap();
    let parent = dir.path().join("main.prompt");
    fs::write(
        &parent,
        format!(r#"{{{{ include "../{sibling_name}/secret.prompt" }}}}"#),
    )
    .unwrap();
    let src = fs::read_to_string(&parent).unwrap();
    let r = render_template_result(&src, None, Some(dir.path()), Some(&parent));
    let _ = fs::remove_dir_all(outside_dir);
    assert!(r.is_err());
    assert!(r.unwrap_err().kind.contains("escapes template root"));
}

fn tempdir() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

// Branch-trace coverage for #1668. The transcript event built on this
// trace is what powers the portal's variant-resolution panel; here we
// assert determinism (same llm snapshot → same trace) plus the
// labels the trace consumer expects.

#[test]
fn branch_trace_records_if_taken_branch() {
    let _guard =
        LlmRenderContextGuard::enter(LlmRenderContext::resolve("anthropic", "claude-opus-4-7"));
    let tpl = "{{ if llm.capabilities.native_tools }}native{{ else }}text{{ end }}";
    let (rendered, trace) = render_template_collect_branch_trace(tpl).unwrap();
    assert_eq!(rendered, "native");
    assert_eq!(trace.len(), 1);
    let decision = &trace[0];
    assert_eq!(decision.kind, BranchKind::If);
    assert_eq!(decision.branch_id, "if");
    assert_eq!(
        decision.branch_label.as_deref(),
        Some("llm.capabilities.native_tools"),
    );
}

#[test]
fn branch_trace_records_else_when_no_branch_matches() {
    let _guard =
        LlmRenderContextGuard::enter(LlmRenderContext::resolve("anthropic", "claude-opus-4-7"));
    let tpl =
        "{{ if llm.provider == \"openai\" }}openai{{ elif llm.provider == \"google\" }}google{{ else }}other{{ end }}";
    let (rendered, trace) = render_template_collect_branch_trace(tpl).unwrap();
    assert_eq!(rendered, "other");
    assert_eq!(trace.len(), 1);
    assert_eq!(trace[0].branch_id, "else");
}

#[test]
fn branch_trace_records_section_envelope() {
    let _guard =
        LlmRenderContextGuard::enter(LlmRenderContext::resolve("anthropic", "claude-opus-4-7"));
    let tpl = "{{ section \"task\" }}Build a tree CLI.{{ endsection }}";
    let (_rendered, trace) = render_template_collect_branch_trace(tpl).unwrap();
    let section = trace
        .iter()
        .find(|d| d.kind == BranchKind::Section)
        .expect("section trace entry");
    assert_eq!(section.branch_id, "xml");
    assert_eq!(section.branch_label.as_deref(), Some("task"));
}

#[test]
fn branch_trace_is_deterministic_across_repeated_renders() {
    let _guard =
        LlmRenderContextGuard::enter(LlmRenderContext::resolve("anthropic", "claude-opus-4-7"));
    let tpl = "\
{{ if llm.capabilities.native_tools }}n{{ end }}\
{{ section \"task\" }}b{{ endsection }}";
    let (_, trace_a) = render_template_collect_branch_trace(tpl).unwrap();
    let (_, trace_b) = render_template_collect_branch_trace(tpl).unwrap();
    assert_eq!(trace_a, trace_b);
}

#[test]
fn branch_trace_label_summarizes_provider_identity_comparison() {
    let _guard = LlmRenderContextGuard::enter(LlmRenderContext::resolve("openai", "gpt-5.4"));
    let tpl = "{{ if llm.provider == \"openai\" }}o{{ else }}x{{ end }}";
    let (_rendered, trace) = render_template_collect_branch_trace(tpl).unwrap();
    assert_eq!(trace.len(), 1);
    assert_eq!(trace[0].branch_id, "if");
    assert_eq!(
        trace[0].branch_label.as_deref(),
        Some("llm.provider == \"openai\""),
    );
}
