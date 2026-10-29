#![expect(clippy::string_slice, reason = "test input is ASCII")]

use super::*;
use crate::value::VmDictExt;
use crate::value::VmValue;

fn dict(pairs: &[(&str, VmValue)]) -> crate::value::DictMap {
    pairs
        .iter()
        .map(|(k, v)| (crate::value::intern_key(k), v.clone()))
        .collect()
}

fn s(v: &str) -> VmValue {
    VmValue::string(v)
}

fn render(tpl: &str, b: &crate::value::DictMap) -> String {
    render_template_result(tpl, Some(b), None, None).unwrap()
}

fn render_with_spans(tpl: &str, b: &crate::value::DictMap) -> (String, Vec<PromptSourceSpan>) {
    render_template_with_provenance(tpl, Some(b), None, None, true).unwrap()
}

fn list(items: Vec<VmValue>) -> VmValue {
    VmValue::list(items)
}

#[test]
fn multibyte_template_bodies_do_not_panic() {
    // Regression: the lexer read fixed byte windows after "{{" (keyword and
    // close-marker probes), which panicked mid-character on non-ASCII input.
    let b = dict(&[]);
    for tpl in ["{{ éé }}", "{{ 😀 }}", "{{ raw😀", "{{éé"] {
        let _ = render_template_result(tpl, Some(&b), None, None);
    }
}

#[test]
fn provenance_expr_span_matches_output_range() {
    let mut user = crate::value::DictMap::new();
    user.put("name", s("alice"));
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
    let b = dict(&[("items", VmValue::list(vec![s("a"), s("b"), s("c")]))]);
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
    wrap.put("val", s(&"x".repeat(500)));
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
fn legacy_if_falsey() {
    let b = dict(&[("x", VmValue::Bool(false))]);
    assert_eq!(render("{{if x}}yes{{end}}", &b), "");
}

#[test]
fn filter_default() {
    let b = dict(&[("n", s(""))]);
    assert_eq!(render("{{n | default: \"anon\"}}", &b), "anon");
}

#[test]
fn comparison_ops() {
    let b = dict(&[("n", VmValue::Int(5))]);
    assert_eq!(render("{{if n > 3}}big{{end}}", &b), "big");
    assert_eq!(render("{{if n >= 5 and n < 10}}ok{{end}}", &b), "ok");
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
    let tool = VmValue::dict([("name", s("read_file")), ("description", s("Read a file"))]);
    let schema = VmValue::dict([
        ("type", s("object")),
        ("properties", VmValue::dict([("answer", s("string"))])),
    ]);
    let b = dict(&[("tools", list(vec![tool])), ("schema", schema)]);

    {
        let _guard = LlmRenderContextGuard::enter(LlmRenderContext::resolve(
            "openrouter",
            "anthropic/claude-opus-4-7",
        ));
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
        let _guard =
            LlmRenderContextGuard::enter(LlmRenderContext::resolve("anthropic", "claude-opus-4-7"));
        let out = render(
            "{{ section \"tools\" tools=tools }}{{ endsection }}\n{{ section \"output_format\" schema=schema }}{{ endsection }}",
            &b,
        );
        assert!(out.contains("<tools>"));
        assert!(out.contains("\"name\": \"read_file\""));
        assert!(!out.contains("<output_format>"));
        assert!(!out.contains("\"answer\": \"string\""));
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
fn filesystem_template_cache_invalidates_when_contents_change() {
    use std::fs;
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
