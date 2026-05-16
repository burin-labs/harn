#![recursion_limit = "256"]

//! In-process coverage for `harn eval prompt` (#1670).
//!
//! Render mode is purely deterministic — no LLM call — so a small
//! capability-branching fixture renders byte-stably across the
//! capability profiles and lets us assert that the auto-injected `llm`
//! scope dispatches the right envelope per model. Run and judge modes
//! require live credentials (or a Harn-side mock harness), so they're
//! exercised by the unit tests next to the implementation rather than
//! here; the conformance template at
//! `conformance/tests/templates/template_llm_scope_inject.harn` keeps
//! the underlying `LlmRenderContext` machinery honest.

use std::fs;
use std::thread;

use harn_cli::cli::{EvalPromptArgs, EvalPromptMode, EvalPromptOutput};

fn run_in_harn_runtime<F, Fut, R>(future_factory: F) -> R
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = R>,
    R: Send + 'static,
{
    let handle = thread::Builder::new()
        .name("harn-eval-prompt-test".to_string())
        .stack_size(harn_cli::CLI_RUNTIME_STACK_SIZE)
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("build runtime");
            runtime.block_on(future_factory())
        })
        .expect("spawn runtime thread");
    handle.join().expect("runtime thread completed")
}

#[test]
fn render_mode_emits_per_capability_envelope_for_four_profiles() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let template = tmp.path().join("system.harn.prompt");
    fs::write(
        &template,
        // Branch on capabilities so each profile materializes a
        // distinct wire envelope. Native-tool models get the tool-call
        // contract; text-tool models get the delimited fallback;
        // anything that supports XML scaffolding gets an XML cue;
        // the rest fall back to markdown.
        "{{ if llm.capabilities.native_tools }}\
native_tools: call finish_task() when done.\n\
{{ else }}\
text_tools: emit `<<DONE>>` when done.\n\
{{ end }}\
provider={{ llm.provider }} family={{ llm.family }}\n",
    )
    .expect("write template");

    let out_file = tmp.path().join("report.json");
    let args = EvalPromptArgs {
        file: template.clone(),
        fleet: vec![
            "claude-3-5-sonnet".to_string(),
            "gpt-4o".to_string(),
            "gemini-1.5-pro".to_string(),
            "ollama:qwen3.5".to_string(),
        ],
        fleet_name: None,
        bindings: None,
        context_fixture: Vec::new(),
        mode: EvalPromptMode::Render,
        output: EvalPromptOutput::Json,
        out_file: Some(out_file.clone()),
        max_concurrent: 1,
        judge_template: None,
        judge_model: "claude-opus-4-7".to_string(),
        max_tokens: 256,
        fail_on_unauthorized: false,
    };

    let exit =
        run_in_harn_runtime(|| async move { harn_cli::commands::eval_prompt::run(args).await });
    assert_eq!(exit, 0, "render-mode exit code");

    let raw = fs::read_to_string(&out_file).expect("report exists");
    let report: serde_json::Value = serde_json::from_str(&raw).expect("parsed JSON");

    let renders = report["renders"].as_array().expect("renders array");
    assert_eq!(renders.len(), 4, "one render per fleet member");

    // Each rendered envelope must be present and reflect the resolved
    // family — that's the load-bearing guarantee of capability-based
    // dispatch (no provider-identity branching in templates).
    let mut families = std::collections::BTreeSet::new();
    for entry in renders {
        assert!(
            entry["error"].is_null(),
            "render error for {:?}: {:?}",
            entry["selector"],
            entry["error"],
        );
        let rendered = entry["rendered"].as_str().expect("rendered string");
        assert!(
            !rendered.is_empty(),
            "rendered output empty for {:?}",
            entry["selector"]
        );
        let family = entry["family"].as_str().expect("family present");
        families.insert(family.to_string());
        assert!(
            rendered.contains(&format!("family={family}")),
            "rendered envelope must echo family {family}: {rendered:?}",
        );
    }
    // claude / gpt / gemini / qwen families — confirms the diff
    // renderer actually exercises distinct capability profiles rather
    // than collapsing every selector to the same envelope.
    assert!(families.contains("claude"));
    assert!(families.contains("gpt"));
    assert!(families.contains("gemini"));
    assert!(families.contains("qwen"));
}

#[test]
fn fleet_resolution_from_harn_toml_fleet_name() {
    let tmp = tempfile::tempdir().expect("tempdir");
    fs::write(
        tmp.path().join("harn.toml"),
        r#"
[eval.fleets.smoke]
models = ["claude-3-5-sonnet", "gpt-4o"]
"#,
    )
    .expect("write harn.toml");

    let template = tmp.path().join("system.harn.prompt");
    fs::write(&template, "model={{ llm.model }} family={{ llm.family }}\n")
        .expect("write template");

    let out_file = tmp.path().join("report.json");
    let args = EvalPromptArgs {
        file: template.clone(),
        fleet: Vec::new(),
        fleet_name: Some("smoke".to_string()),
        bindings: None,
        context_fixture: Vec::new(),
        mode: EvalPromptMode::Render,
        output: EvalPromptOutput::Json,
        out_file: Some(out_file.clone()),
        max_concurrent: 1,
        judge_template: None,
        judge_model: "claude-opus-4-7".to_string(),
        max_tokens: 256,
        fail_on_unauthorized: false,
    };

    let exit =
        run_in_harn_runtime(|| async move { harn_cli::commands::eval_prompt::run(args).await });
    assert_eq!(exit, 0, "named-fleet exit code");

    let raw = fs::read_to_string(&out_file).expect("report exists");
    let report: serde_json::Value = serde_json::from_str(&raw).expect("parsed JSON");
    assert_eq!(
        report["renders"]
            .as_array()
            .map(|r| r.len())
            .unwrap_or_default(),
        2,
        "fleet from harn.toml resolved",
    );
}

#[test]
fn missing_fleet_name_reports_known_fleets() {
    let tmp = tempfile::tempdir().expect("tempdir");
    fs::write(
        tmp.path().join("harn.toml"),
        r#"
[eval.fleets.frontier]
models = ["claude-3-5-sonnet"]
"#,
    )
    .expect("write harn.toml");
    let template = tmp.path().join("p.harn.prompt");
    fs::write(&template, "x={{ llm.provider }}\n").expect("write template");

    // Pull just the resolver out of the public surface by running a
    // render with an unknown fleet name; we should get exit code 2 and
    // the suggested-name list goes to stderr (verified by smoke).
    let args = EvalPromptArgs {
        file: template,
        fleet: Vec::new(),
        fleet_name: Some("nope".to_string()),
        bindings: None,
        context_fixture: Vec::new(),
        mode: EvalPromptMode::Render,
        output: EvalPromptOutput::Terminal,
        out_file: None,
        max_concurrent: 1,
        judge_template: None,
        judge_model: "claude-opus-4-7".to_string(),
        max_tokens: 256,
        fail_on_unauthorized: false,
    };

    let exit =
        run_in_harn_runtime(|| async move { harn_cli::commands::eval_prompt::run(args).await });
    assert_eq!(exit, 2, "unknown fleet should exit 2");
}

#[test]
fn bindings_file_drives_template_scope() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let template = tmp.path().join("p.harn.prompt");
    fs::write(&template, "task={{ task }}\n").expect("write template");
    let bindings = tmp.path().join("bindings.json");
    fs::write(&bindings, r#"{"task": "Hello from bindings"}"#).expect("write bindings");
    let out_file = tmp.path().join("report.json");

    let args = EvalPromptArgs {
        file: template,
        fleet: vec!["claude-3-5-sonnet".to_string()],
        fleet_name: None,
        bindings: Some(bindings),
        context_fixture: Vec::new(),
        mode: EvalPromptMode::Render,
        output: EvalPromptOutput::Json,
        out_file: Some(out_file.clone()),
        max_concurrent: 1,
        judge_template: None,
        judge_model: "claude-opus-4-7".to_string(),
        max_tokens: 256,
        fail_on_unauthorized: false,
    };

    let exit =
        run_in_harn_runtime(|| async move { harn_cli::commands::eval_prompt::run(args).await });
    assert_eq!(exit, 0);

    let report: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&out_file).expect("report")).expect("parsed JSON");
    let rendered = report["renders"][0]["rendered"]
        .as_str()
        .expect("rendered")
        .to_string();
    assert_eq!(rendered, "task=Hello from bindings\n", "bindings injected");
}

#[test]
fn context_fixture_scores_artifact_selection_and_section_shape() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let template = tmp.path().join("repo-context.harn.prompt");
    fs::write(
        &template,
        "{{ section \"system_framing\" }}Use only selected repository context.{{ endsection }}\n\
{{ section \"task\" }}{{ task }}{{ endsection }}\n\
{{ section \"examples\" }}{{ context }}{{ endsection }}\n",
    )
    .expect("write template");

    let fixture = tmp.path().join("context-fixture.json");
    fs::write(
        &fixture,
        r#"{
  "_type": "prompt_context_eval_fixture",
  "version": 1,
  "id": "repo_context_quality",
  "cases": [
    {
      "id": "rust_parser_regression",
      "bindings": {
        "task": "Fix the Rust parser token span regression."
      },
      "assembler": {
        "budget_tokens": 18,
        "dedup": "none",
        "strategy": "relevance",
        "query": "Rust parser token span regression",
        "microcompact_threshold": 2000
      },
      "artifacts": [
        {
          "id": "rust-parser-current",
          "kind": "workspace_file",
          "title": "Rust parser current",
          "source": "crates/harn-parser/src/parser.rs",
          "created_at": "2026-05-16T00:00:00Z",
          "freshness": "fresh",
          "text": "Rust parser token span regression fix in harn-parser."
        },
        {
          "id": "rust-parser-stale",
          "kind": "workspace_file",
          "title": "Stale parser note",
          "source": "notes/parser.md",
          "created_at": "2025-01-01T00:00:00Z",
          "freshness": "stale",
          "text": "Old parser note before the diagnostic rewrite. Do not use for the current Rust regression."
        },
        {
          "id": "swift-wrong-language",
          "kind": "workspace_file",
          "title": "Swift scanner notes",
          "source": "Sources/BurinCore/Scanner.swift",
          "created_at": "2026-05-16T00:00:00Z",
          "freshness": "fresh",
          "text": "Swift scanner token span advice for a different host implementation."
        },
        {
          "id": "ci-noisy",
          "kind": "command_result",
          "title": "Tempting CI noise",
          "source": "ci.log",
          "created_at": "2026-05-16T00:00:00Z",
          "freshness": "fresh",
          "text": "Rust parser troubleshooting log: unrelated macOS sandbox output, retry chatter, and dependency download noise."
        }
      ],
      "expect": {
        "selected_artifact_ids": ["rust-parser-current"],
        "rejected_artifact_ids": ["rust-parser-stale", "swift-wrong-language", "ci-noisy"],
        "stale_artifact_ids": ["rust-parser-stale"],
        "max_total_tokens": 18,
        "required_section_names": ["system_framing", "task", "examples"],
        "section_envelopes_by_family": {
          "claude": {"system_framing": "xml", "task": "xml", "examples": "xml"},
          "gpt": {"system_framing": "markdown", "task": "markdown", "examples": "markdown"},
          "qwen": {"system_framing": "markdown", "task": "markdown", "examples": "markdown"}
        }
      }
    }
  ]
}"#,
    )
    .expect("write fixture");

    let out_file = tmp.path().join("report.json");
    let args = EvalPromptArgs {
        file: template,
        fleet: vec![
            "claude-3-5-sonnet".to_string(),
            "gpt-4o".to_string(),
            "ollama:qwen3.5".to_string(),
        ],
        fleet_name: None,
        bindings: None,
        context_fixture: vec![fixture],
        mode: EvalPromptMode::Render,
        output: EvalPromptOutput::Json,
        out_file: Some(out_file.clone()),
        max_concurrent: 1,
        judge_template: None,
        judge_model: "claude-opus-4-7".to_string(),
        max_tokens: 256,
        fail_on_unauthorized: false,
    };

    let exit =
        run_in_harn_runtime(|| async move { harn_cli::commands::eval_prompt::run(args).await });
    assert_eq!(exit, 0, "context fixture eval should pass");

    let report: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&out_file).expect("report")).expect("parsed JSON");
    let context_eval = &report["context_eval"];
    assert_eq!(context_eval["pass"], true);
    let case = &context_eval["fixtures"][0]["cases"][0];
    assert_eq!(case["selected_artifact_ids"][0], "rust-parser-current");
    assert_eq!(case["score"]["overall"], 1.0);
    assert_eq!(case["score"]["selected_artifacts"], 1.0);
    assert_eq!(case["score"]["rejected_artifacts"], 1.0);
    assert_eq!(case["score"]["stale_rejection"], 1.0);
    assert_eq!(case["score"]["token_budget"], 1.0);
    assert_eq!(case["score"]["rendered_sections"], 1.0);
    assert_eq!(case["budget"]["pass"], true);
    let variants = case["variants"].as_array().expect("variants");
    assert_eq!(variants.len(), 3);
    for variant in variants {
        assert_eq!(
            variant["pass"], true,
            "variant should satisfy section shape: {variant:?}",
        );
    }
    assert_eq!(variants[0]["sections"][0]["envelope"], "xml");
    assert_eq!(variants[1]["sections"][0]["envelope"], "markdown");
    assert_eq!(variants[2]["sections"][0]["envelope"], "markdown");
}
