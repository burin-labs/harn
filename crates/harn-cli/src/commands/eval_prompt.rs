//! `harn eval prompt <file> --fleet <models>` — render and optionally run
//! a single `.harn.prompt` template across a fleet of models so authors
//! can validate the capability-adapted envelope per model side-by-side.
//!
//! Render mode is the v1 acceptance path: it pushes an LLM render
//! context per model, calls the template engine, and emits the rendered
//! envelope plus a wire-format diff. Run and judge modes synthesize a
//! tiny Harn driver and route through the existing `execute_run`
//! pipeline so credentialed LLM calls, mock fixtures, and the
//! `LlmRenderContext` injection all stay on the canonical path.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use harn_vm::llm_config;
use harn_vm::stdlib::template::{
    render_template_to_string_with_branch_trace, BranchDecision, LlmRenderContext,
    LlmRenderContextGuard,
};
use harn_vm::value::VmValue;
use serde_json::Value as JsonValue;

use crate::cli::{EvalPromptArgs, EvalPromptMode, EvalPromptOutput};
use crate::config;

use super::eval_prompt_context::{evaluate_context_fixtures, PromptContextEvalReport};

/// Resolved per-model envelope produced by `--mode render`.
#[derive(Debug, Clone, serde::Serialize)]
struct ModelRender {
    /// User-supplied selector (alias or `provider:model`).
    selector: String,
    provider: String,
    model: String,
    family: String,
    capabilities: JsonValue,
    /// `Some` on success; `None` if template rendering failed.
    rendered: Option<String>,
    error: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    branches: Vec<TemplateBranch>,
    /// `true` when the model's provider has no usable credentials. In
    /// render mode this is informational; in run/judge mode it controls
    /// whether the call is skipped.
    auth_available: bool,
}

/// Per-model artifact produced by `--mode run`.
#[derive(Debug, Clone, serde::Serialize, Default)]
struct ModelRunResult {
    response: Option<String>,
    error: Option<String>,
    /// True if the call was skipped because the provider was unauthenticated.
    skipped: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
struct PromptReport {
    template_path: PathBuf,
    mode: &'static str,
    renders: Vec<ModelRender>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    runs: BTreeMap<String, ModelRunResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    judge: Option<JudgeReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    context_eval: Option<PromptContextEvalReport>,
}

#[derive(Debug, Clone, serde::Serialize)]
struct JudgeReport {
    judge_model: String,
    /// Raw judge response text — the built-in judge template asks for a
    /// short JSON or prose verdict; we surface it verbatim so the user
    /// can inspect.
    verdict: String,
}

#[derive(Debug, Clone, serde::Serialize)]
struct TemplateBranch {
    kind: String,
    template_uri: String,
    line: usize,
    col: usize,
    branch_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    branch_label: Option<String>,
}

impl From<&BranchDecision> for TemplateBranch {
    fn from(decision: &BranchDecision) -> Self {
        Self {
            kind: decision.kind.as_str().to_string(),
            template_uri: decision.template_uri.clone(),
            line: decision.line,
            col: decision.col,
            branch_id: decision.branch_id.clone(),
            branch_label: decision.branch_label.clone(),
        }
    }
}

pub async fn run(args: EvalPromptArgs) -> i32 {
    let template_path = match fs::canonicalize(&args.file) {
        Ok(p) => p,
        Err(error) => {
            eprintln!(
                "error: cannot resolve template path {}: {error}",
                args.file.display()
            );
            return 1;
        }
    };
    let template_source = match fs::read_to_string(&template_path) {
        Ok(s) => s,
        Err(error) => {
            eprintln!("error: failed to read {}: {error}", template_path.display());
            return 1;
        }
    };

    let fleet = match resolve_fleet(&args, &template_path) {
        Ok(f) => f,
        Err(error) => {
            eprintln!("error: {error}");
            return 2;
        }
    };
    if fleet.is_empty() {
        eprintln!("error: fleet is empty — supply `--fleet <models>` or `--fleet-name <name>`");
        return 2;
    }

    let bindings = match load_bindings(args.bindings.as_deref()) {
        Ok(b) => b,
        Err(error) => {
            eprintln!("error: {error}");
            return 1;
        }
    };

    let renders = render_fleet(&fleet, &template_source, &template_path, bindings.as_ref());

    let mode = args.mode;
    let mut report = PromptReport {
        template_path: template_path.clone(),
        mode: mode_label(mode),
        renders,
        runs: BTreeMap::new(),
        judge: None,
        context_eval: None,
    };

    if !args.context_fixture.is_empty() {
        match evaluate_context_fixtures(
            &args.context_fixture,
            &fleet,
            &template_source,
            &template_path,
            bindings.as_ref(),
        ) {
            Ok(context_eval) => report.context_eval = Some(context_eval),
            Err(error) => {
                eprintln!("error: {error}");
                return 1;
            }
        }
    }

    if matches!(mode, EvalPromptMode::Run | EvalPromptMode::Judge) {
        let bindings_text = args
            .bindings
            .as_ref()
            .map(|p| p.to_string_lossy().to_string());
        let outputs = execute_runs(
            &report.renders,
            &template_path,
            bindings_text.as_deref(),
            args.max_tokens,
            args.max_concurrent,
            args.fail_on_unauthorized,
        )
        .await;
        match outputs {
            Ok(map) => report.runs = map,
            Err(code) => return code,
        }
    }

    if matches!(mode, EvalPromptMode::Judge) {
        match execute_judge(
            &report,
            args.judge_template.as_deref(),
            &args.judge_model,
            args.max_tokens,
        )
        .await
        {
            Ok(judge) => report.judge = Some(judge),
            Err(code) => return code,
        }
    }

    let payload = match args.output {
        EvalPromptOutput::Terminal => render_terminal(&report),
        EvalPromptOutput::Json => render_json(&report),
        EvalPromptOutput::Html => render_html(&report),
    };

    match args.out_file {
        Some(path) => {
            if let Err(error) = fs::write(&path, payload) {
                eprintln!("error: failed to write {}: {error}", path.display());
                return 1;
            }
            eprintln!("wrote {}", path.display());
        }
        None => {
            let mut stdout = std::io::stdout().lock();
            let _ = stdout.write_all(payload.as_bytes());
        }
    }

    let context_eval_active = report.context_eval.is_some();
    if !context_eval_active && report.renders.iter().any(|r| r.error.is_some()) {
        return 1;
    }
    if report.runs.values().any(|r| r.error.is_some()) {
        return 1;
    }
    if report
        .context_eval
        .as_ref()
        .is_some_and(|context_eval| !context_eval.pass)
    {
        return 1;
    }
    0
}

fn mode_label(mode: EvalPromptMode) -> &'static str {
    match mode {
        EvalPromptMode::Render => "render",
        EvalPromptMode::Run => "run",
        EvalPromptMode::Judge => "judge",
    }
}

/// Resolve the fleet entries from `--fleet` / `--fleet-name`, expanding
/// aliases through `llm_config::resolve_model_info` so downstream code
/// works with `(provider, model)` pairs regardless of input shape.
fn resolve_fleet(args: &EvalPromptArgs, template_path: &Path) -> Result<Vec<FleetEntry>, String> {
    let raw_selectors: Vec<String> = if let Some(name) = args.fleet_name.as_ref() {
        let cfg = config::load_for_path(template_path)
            .map_err(|error| format!("failed to load harn.toml: {error}"))?;
        let Some(fleet) = cfg.eval.fleets.get(name) else {
            let available: Vec<&str> = cfg.eval.fleets.keys().map(|s| s.as_str()).collect();
            return Err(if available.is_empty() {
                format!("unknown fleet `{name}` — no `[eval.fleets.*]` entries found in harn.toml",)
            } else {
                format!(
                    "unknown fleet `{name}` — known fleets: {}",
                    available.join(", "),
                )
            });
        };
        fleet.models.clone()
    } else {
        args.fleet.clone()
    };

    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for selector in raw_selectors {
        let trimmed = selector.trim();
        if trimmed.is_empty() {
            continue;
        }
        if !seen.insert(trimmed.to_string()) {
            continue;
        }
        let resolved = llm_config::resolve_model_info(trimmed);
        out.push(FleetEntry {
            selector: trimmed.to_string(),
            provider: resolved.provider,
            model: resolved.id,
        });
    }
    Ok(out)
}

#[derive(Debug, Clone)]
pub(crate) struct FleetEntry {
    pub(crate) selector: String,
    pub(crate) provider: String,
    pub(crate) model: String,
}

fn load_bindings(path: Option<&Path>) -> Result<Option<VmValue>, String> {
    let Some(path) = path else {
        return Ok(None);
    };
    let raw = fs::read_to_string(path)
        .map_err(|error| format!("failed to read bindings {}: {error}", path.display()))?;
    let json: JsonValue = serde_json::from_str(&raw)
        .map_err(|error| format!("failed to parse bindings {}: {error}", path.display()))?;
    if !json.is_object() {
        return Err(format!(
            "bindings file {} must be a JSON object at the top level",
            path.display(),
        ));
    }
    Ok(Some(harn_vm::json_to_vm_value(&json)))
}

fn render_fleet(
    fleet: &[FleetEntry],
    template_source: &str,
    template_path: &Path,
    bindings: Option<&VmValue>,
) -> Vec<ModelRender> {
    let base = template_path.parent();
    let bindings_dict: Option<BTreeMap<String, VmValue>> = bindings.and_then(|v| match v {
        VmValue::Dict(dict) => Some(dict.as_ref().clone()),
        _ => None,
    });

    fleet
        .iter()
        .map(|entry| {
            // Resolve a fresh capability snapshot per model so the
            // `llm` scope inside the template reflects the model under
            // evaluation rather than a stale parent frame.
            let ctx = LlmRenderContext::resolve(&entry.provider, &entry.model);
            let family = ctx.family.clone();
            let capabilities = vm_value_to_json(&ctx.capabilities);
            let auth_available = llm_config::provider_key_available(&entry.provider);

            let result = {
                let _guard = LlmRenderContextGuard::enter(ctx);
                render_template_to_string_with_branch_trace(
                    template_source,
                    bindings_dict.as_ref(),
                    base,
                    Some(template_path),
                )
            };

            let (rendered, branches, error) = match result {
                Ok((text, trace)) => (
                    Some(text),
                    trace.iter().map(TemplateBranch::from).collect(),
                    None,
                ),
                Err(message) => (None, Vec::new(), Some(message)),
            };

            ModelRender {
                selector: entry.selector.clone(),
                provider: entry.provider.clone(),
                model: entry.model.clone(),
                family,
                capabilities,
                rendered,
                error,
                branches,
                auth_available,
            }
        })
        .collect()
}

fn vm_value_to_json(value: &VmValue) -> JsonValue {
    match value {
        VmValue::Nil => JsonValue::Null,
        VmValue::Bool(b) => JsonValue::Bool(*b),
        VmValue::Int(i) => JsonValue::Number((*i).into()),
        VmValue::Float(f) => serde_json::Number::from_f64(*f)
            .map(JsonValue::Number)
            .unwrap_or(JsonValue::Null),
        VmValue::String(s) => JsonValue::String(s.to_string()),
        VmValue::List(items) => JsonValue::Array(items.iter().map(vm_value_to_json).collect()),
        VmValue::Dict(d) => {
            let mut map = serde_json::Map::new();
            for (k, v) in d.iter() {
                map.insert(k.clone(), vm_value_to_json(v));
            }
            JsonValue::Object(map)
        }
        // Anything else (closures, handles, etc.) is unlikely to appear
        // in a capability snapshot but we surface a sentinel so callers
        // don't crash on a future-added kind.
        other => JsonValue::String(format!("<{}>", other.type_name())),
    }
}

// ─── Run mode ──────────────────────────────────────────────────────────────

async fn execute_runs(
    renders: &[ModelRender],
    template_path: &Path,
    bindings_path: Option<&str>,
    max_tokens: i64,
    max_concurrent: usize,
    fail_on_unauthorized: bool,
) -> Result<BTreeMap<String, ModelRunResult>, i32> {
    let mut runnable: Vec<&ModelRender> = Vec::new();
    let mut runs: BTreeMap<String, ModelRunResult> = BTreeMap::new();
    let mock_active = std::env::var("HARN_LLM_PROVIDER")
        .map(|v| v == "mock")
        .unwrap_or(false);
    for render in renders {
        if render.error.is_some() {
            runs.insert(
                render.selector.clone(),
                ModelRunResult {
                    error: Some("template render failed — see render section".to_string()),
                    ..Default::default()
                },
            );
            continue;
        }
        if !mock_active && !render.auth_available {
            if fail_on_unauthorized {
                eprintln!(
                    "error: provider `{}` (for `{}`) has no credentials configured",
                    render.provider, render.selector,
                );
                return Err(1);
            }
            eprintln!(
                "warn: provider `{}` (for `{}`) unauthenticated — skipping run",
                render.provider, render.selector,
            );
            runs.insert(
                render.selector.clone(),
                ModelRunResult {
                    skipped: true,
                    ..Default::default()
                },
            );
            continue;
        }
        runnable.push(render);
    }
    if runnable.is_empty() {
        return Ok(runs);
    }

    let script = build_run_script(
        &runnable,
        template_path,
        bindings_path,
        max_tokens,
        max_concurrent.max(1),
    );
    let outputs = match invoke_harn_script(&script).await {
        Ok(out) => out,
        Err(err) => {
            eprintln!("error: run-mode harn script failed: {err}");
            return Err(1);
        }
    };
    for line in outputs.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let entry: HarnRunLine = match serde_json::from_str(line) {
            Ok(e) => e,
            Err(_) => continue,
        };
        let result = ModelRunResult {
            response: entry.response,
            error: entry.error,
            skipped: false,
        };
        runs.insert(entry.selector, result);
    }
    Ok(runs)
}

#[derive(Debug, serde::Deserialize)]
struct HarnRunLine {
    selector: String,
    #[serde(default)]
    response: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

fn build_run_script(
    fleet: &[&ModelRender],
    template_path: &Path,
    bindings_path: Option<&str>,
    max_tokens: i64,
    _max_concurrent: usize,
) -> String {
    // Sequential dispatch: `llm_call` pushes its own `LlmRenderContext`
    // guard for the duration of each call and asserts strict LIFO drop
    // ordering against the shared thread-local template stack. Running
    // multiple `llm_call`s concurrently on the VM's LocalSet interleaves
    // those pushes and trips the guard. `--max-concurrent` is accepted
    // for forward compatibility with a future per-provider parallelism
    // path; today it is a no-op.
    let template_path_lit = json_string_literal(&template_path.to_string_lossy());
    let bindings_load = if let Some(path) = bindings_path {
        let path_lit = json_string_literal(path);
        format!("    let bindings = json_parse(read_file({path_lit}))\n")
    } else {
        "    let bindings = {}\n".to_string()
    };
    let fleet_items: Vec<String> = fleet
        .iter()
        .map(|r| {
            format!(
                "        {{selector: {}, provider: {}, model: {}}}",
                json_string_literal(&r.selector),
                json_string_literal(&r.provider),
                json_string_literal(&r.model),
            )
        })
        .collect();
    let fleet_list = if fleet_items.is_empty() {
        "[]".to_string()
    } else {
        format!("[\n{}\n    ]", fleet_items.join(",\n"))
    };

    format!(
        "pipeline main() {{\n\
{bindings_load}\
    let fleet = {fleet_list}\n\
    for entry in fleet {{\n\
        let pushed = __push_llm_render_context(entry.provider, entry.model)\n\
        let rendered = render({template_path_lit}, bindings)\n\
        try {{\n\
            let resp = llm_call(rendered, nil, {{\n\
                provider: entry.provider,\n\
                model: entry.model,\n\
                max_tokens: {max_tokens}\n\
            }})\n\
            __io_println(json_stringify({{selector: entry.selector, response: resp}}))\n\
        }} catch (err) {{\n\
            __io_println(json_stringify({{selector: entry.selector, error: to_string(err)}}))\n\
        }}\n\
        if pushed {{\n\
            __pop_llm_render_context()\n\
        }}\n\
    }}\n\
}}\n",
    )
}

async fn invoke_harn_script(script: &str) -> Result<String, String> {
    use std::collections::HashSet;
    let tmp = tempfile::Builder::new()
        .prefix("harn-eval-prompt-")
        .suffix(".harn")
        .tempfile()
        .map_err(|e| format!("tempfile: {e}"))?;
    fs::write(tmp.path(), script).map_err(|e| format!("write tempfile: {e}"))?;

    let outcome = crate::commands::run::execute_run(
        &tmp.path().to_string_lossy(),
        false,
        HashSet::new(),
        Vec::new(),
        Vec::new(),
        crate::commands::run::CliLlmMockMode::Off,
        None,
        crate::commands::run::RunProfileOptions::default(),
    )
    .await;

    if outcome.exit_code != 0 {
        return Err(format!(
            "harn run exited {} — stderr:\n{}",
            outcome.exit_code, outcome.stderr,
        ));
    }
    Ok(outcome.stdout)
}

fn json_string_literal(value: &str) -> String {
    serde_json::Value::String(value.to_string()).to_string()
}

// ─── Judge mode ────────────────────────────────────────────────────────────

const DEFAULT_JUDGE_TEMPLATE: &str = r#"You are a strict-equivalence judge for prompt-engineering output.

The same logical prompt was rendered for several models and each model returned a response. Your task is to determine whether the responses are *semantically equivalent* — the wire envelope may differ (XML vs markdown vs native tool calls), but the user-facing intent and information content should be the same.

Source prompt template (for context):

{{ template_source }}

Per-model responses:
{{ for entry in entries }}
---
model: {{ entry.selector }} (provider={{ entry.provider }}, family={{ entry.family }})

rendered prompt:
{{ entry.rendered }}

response:
{{ entry.response }}
{{ end }}

Reply with a short JSON object on a single line of the form:
{"equivalent": true|false, "differences": ["..."], "notes": "..."}
"#;

async fn execute_judge(
    report: &PromptReport,
    judge_template: Option<&Path>,
    judge_model: &str,
    max_tokens: i64,
) -> Result<JudgeReport, i32> {
    let judge_template_body = match judge_template {
        Some(path) => fs::read_to_string(path).map_err(|error| {
            eprintln!(
                "error: failed to read judge template {}: {error}",
                path.display()
            );
            1i32
        })?,
        None => DEFAULT_JUDGE_TEMPLATE.to_string(),
    };
    let prompt_source = fs::read_to_string(&report.template_path).unwrap_or_default();

    let entries: Vec<JudgeEntry> = report
        .renders
        .iter()
        .map(|r| JudgeEntry {
            selector: r.selector.clone(),
            provider: r.provider.clone(),
            family: r.family.clone(),
            rendered: r.rendered.clone().unwrap_or_default(),
            response: report
                .runs
                .get(&r.selector)
                .and_then(|run| run.response.clone())
                .unwrap_or_else(|| "<no response>".to_string()),
        })
        .collect();

    let entries_json = serde_json::to_string(&entries).unwrap_or_else(|_| "[]".to_string());
    let template_lit = json_string_literal(&judge_template_body);
    let entries_lit = json_string_literal(&entries_json);
    let source_lit = json_string_literal(&prompt_source);

    let resolved_judge = llm_config::resolve_model_info(judge_model);
    let provider_lit = json_string_literal(&resolved_judge.provider);
    let model_lit = json_string_literal(&resolved_judge.id);

    let script = format!(
        "pipeline main() {{\n\
    let entries = json_parse({entries_lit})\n\
    let prompt = render_string({template_lit}, {{\n\
        template_source: {source_lit},\n\
        entries: entries\n\
    }})\n\
    let verdict = llm_call(prompt, nil, {{\n\
        provider: {provider_lit},\n\
        model: {model_lit},\n\
        max_tokens: {max_tokens}\n\
    }})\n\
    __io_println(verdict)\n\
}}\n",
    );

    let verdict = match invoke_harn_script(&script).await {
        Ok(out) => out.trim().to_string(),
        Err(err) => {
            eprintln!("error: judge-mode harn script failed: {err}");
            return Err(1);
        }
    };

    Ok(JudgeReport {
        judge_model: judge_model.to_string(),
        verdict,
    })
}

#[derive(Debug, serde::Serialize)]
struct JudgeEntry {
    selector: String,
    provider: String,
    family: String,
    rendered: String,
    response: String,
}

// ─── Output rendering ──────────────────────────────────────────────────────

fn render_terminal(report: &PromptReport) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# harn eval prompt — {} (mode: {})\n\n",
        report.template_path.display(),
        report.mode,
    ));

    let baseline_lines: Option<Vec<&str>> = report
        .renders
        .iter()
        .find_map(|r| r.rendered.as_deref())
        .map(|s| s.lines().collect());

    for (idx, render) in report.renders.iter().enumerate() {
        out.push_str(&format!(
            "## [{idx}] {} ({}/{})  family={}\n",
            render.selector, render.provider, render.model, render.family,
        ));
        if !render.auth_available {
            out.push_str("    auth: not configured\n");
        }
        if let Some(error) = render.error.as_ref() {
            out.push_str(&format!("    render error: {error}\n\n"));
            continue;
        }
        let Some(rendered) = render.rendered.as_deref() else {
            continue;
        };
        out.push_str("---\n");
        out.push_str(rendered);
        if !rendered.ends_with('\n') {
            out.push('\n');
        }
        out.push_str("---\n");
        // Annotate diff vs the first non-error render so the user can
        // eyeball wire-format drift without scanning the full body.
        if idx > 0 {
            if let Some(baseline) = baseline_lines.as_deref() {
                let summary = line_diff_summary(baseline, &rendered.lines().collect::<Vec<_>>());
                if !summary.is_empty() {
                    out.push_str(&format!("    diff vs #0: {summary}\n"));
                }
            }
        }
        out.push('\n');
    }

    if let Some(context_eval) = report.context_eval.as_ref() {
        out.push_str(&format!(
            "\n# Context fixture gates: {} passed / {} total\n",
            context_eval.passed, context_eval.total,
        ));
        for fixture in &context_eval.fixtures {
            out.push_str(&format!(
                "\n## {} ({} passed / {} total)\n",
                fixture.path.display(),
                fixture.passed,
                fixture.total,
            ));
            for case in &fixture.cases {
                out.push_str(&format!(
                    "- {}: {} score={:.3} selected=[{}] tokens={}/{}\n",
                    case.id,
                    if case.pass { "pass" } else { "fail" },
                    case.score.overall,
                    case.selected_artifact_ids.join(", "),
                    case.budget.total_tokens,
                    case.budget.budget_tokens,
                ));
                for failure in &case.failures {
                    out.push_str(&format!("    failure: {failure}\n"));
                }
            }
        }
    }

    if !report.runs.is_empty() {
        out.push_str("\n# Model responses\n");
        for render in &report.renders {
            let Some(run) = report.runs.get(&render.selector) else {
                continue;
            };
            out.push_str(&format!("\n## {} ({})\n", render.selector, render.model));
            if run.skipped {
                out.push_str("    skipped: unauthenticated provider\n");
                continue;
            }
            if let Some(error) = run.error.as_ref() {
                out.push_str(&format!("    error: {error}\n"));
                continue;
            }
            if let Some(response) = run.response.as_deref() {
                out.push_str("---\n");
                out.push_str(response);
                if !response.ends_with('\n') {
                    out.push('\n');
                }
                out.push_str("---\n");
            }
        }
    }

    if let Some(judge) = report.judge.as_ref() {
        out.push_str(&format!(
            "\n# Judge verdict ({}): \n{}\n",
            judge.judge_model, judge.verdict,
        ));
    }

    out
}

/// Summarize line-level differences between two renders without pulling
/// in a full diff dependency. Reports counts of unique lines on either
/// side; the body is already printed adjacently so the reader can dig in
/// from there.
fn line_diff_summary(baseline: &[&str], candidate: &[&str]) -> String {
    let baseline_set: BTreeSet<&str> = baseline.iter().copied().collect();
    let candidate_set: BTreeSet<&str> = candidate.iter().copied().collect();
    let only_in_baseline = baseline_set.difference(&candidate_set).count();
    let only_in_candidate = candidate_set.difference(&baseline_set).count();
    if only_in_baseline == 0 && only_in_candidate == 0 {
        let total_baseline = baseline.len();
        let total_candidate = candidate.len();
        if total_baseline == total_candidate {
            String::new()
        } else {
            format!(
                "{} vs {} lines (same content set, different ordering or repeats)",
                total_baseline, total_candidate,
            )
        }
    } else {
        format!(
            "{} line(s) only in baseline, {} line(s) only here",
            only_in_baseline, only_in_candidate,
        )
    }
}

fn render_json(report: &PromptReport) -> String {
    match serde_json::to_string_pretty(report) {
        Ok(s) => format!("{s}\n"),
        Err(error) => format!("{{\"error\": \"serialize: {error}\"}}\n"),
    }
}

fn render_html(report: &PromptReport) -> String {
    let mut out = String::new();
    out.push_str(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>harn eval prompt report</title>",
    );
    out.push_str(
        "<style>body{font-family:system-ui,sans-serif;margin:2rem;color:#222}h1{margin-bottom:0}",
    );
    out.push_str(".meta{color:#666;font-size:0.9rem;margin-bottom:1.5rem}");
    out.push_str(
        ".grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(28rem,1fr));gap:1rem}",
    );
    out.push_str(".card{border:1px solid #ddd;border-radius:6px;padding:1rem;background:#fafafa}");
    out.push_str(".card h2{margin-top:0;font-size:1rem}");
    out.push_str("pre{background:#fff;border:1px solid #eee;padding:0.75rem;overflow:auto;white-space:pre-wrap;font-size:0.85rem}");
    out.push_str(".err{color:#b00}.skip{color:#888;font-style:italic}");
    out.push_str("</style></head><body>");
    out.push_str(&format!(
        "<h1>harn eval prompt</h1><div class=\"meta\">{} · mode: {}</div>",
        html_escape(&report.template_path.to_string_lossy()),
        report.mode,
    ));
    out.push_str("<div class=\"grid\">");
    for render in &report.renders {
        out.push_str(&format!(
            "<div class=\"card\"><h2>{} <span class=\"meta\">({} / {} · {})</span></h2>",
            html_escape(&render.selector),
            html_escape(&render.provider),
            html_escape(&render.model),
            html_escape(&render.family),
        ));
        if !render.auth_available {
            out.push_str("<p class=\"skip\">auth: not configured</p>");
        }
        match (&render.rendered, &render.error) {
            (_, Some(error)) => {
                out.push_str(&format!(
                    "<p class=\"err\">render error: {}</p>",
                    html_escape(error)
                ));
            }
            (Some(rendered), _) => {
                out.push_str(&format!("<pre>{}</pre>", html_escape(rendered)));
            }
            _ => {}
        }
        if let Some(run) = report.runs.get(&render.selector) {
            if run.skipped {
                out.push_str("<p class=\"skip\">run: skipped (no credentials)</p>");
            } else if let Some(err) = run.error.as_ref() {
                out.push_str(&format!(
                    "<p class=\"err\">run error: {}</p>",
                    html_escape(err)
                ));
            } else if let Some(response) = run.response.as_ref() {
                out.push_str("<h3>response</h3>");
                out.push_str(&format!("<pre>{}</pre>", html_escape(response)));
            }
        }
        out.push_str("</div>");
    }
    out.push_str("</div>");
    if let Some(context_eval) = report.context_eval.as_ref() {
        out.push_str(&format!(
            "<h2>Context fixture gates</h2><p>{} passed / {} total</p>",
            context_eval.passed, context_eval.total,
        ));
        for fixture in &context_eval.fixtures {
            out.push_str(&format!(
                "<h3>{}</h3><ul>",
                html_escape(&fixture.path.to_string_lossy()),
            ));
            for case in &fixture.cases {
                out.push_str(&format!(
                    "<li><strong>{}</strong>: {} · score {:.3} · selected [{}] · tokens {}/{}</li>",
                    html_escape(&case.id),
                    if case.pass { "pass" } else { "fail" },
                    case.score.overall,
                    html_escape(&case.selected_artifact_ids.join(", ")),
                    case.budget.total_tokens,
                    case.budget.budget_tokens,
                ));
            }
            out.push_str("</ul>");
        }
    }
    if let Some(judge) = report.judge.as_ref() {
        out.push_str(&format!(
            "<h2>Judge ({})</h2><pre>{}</pre>",
            html_escape(&judge.judge_model),
            html_escape(&judge.verdict),
        ));
    }
    out.push_str("</body></html>\n");
    out
}

fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fleet_resolution_dedupes_and_expands_aliases() {
        let args = EvalPromptArgs {
            file: PathBuf::from("/tmp/missing.harn.prompt"),
            fleet: vec![
                "claude-3-5-sonnet".to_string(),
                "claude-3-5-sonnet".to_string(),
                "ollama:qwen3.5".to_string(),
            ],
            fleet_name: None,
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
        let entries = resolve_fleet(&args, Path::new("/tmp")).expect("resolve");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].selector, "claude-3-5-sonnet");
        assert_eq!(entries[1].selector, "ollama:qwen3.5");
        assert_eq!(entries[1].provider, "ollama");
        assert_eq!(entries[1].model, "qwen3.5");
    }

    #[test]
    fn render_fleet_emits_per_capability_envelope() {
        let template = "{{ if llm.capabilities.native_tools }}native{{ else }}text{{ end }}\n";
        let fleet = vec![FleetEntry {
            selector: "ollama:qwen3.5".to_string(),
            provider: "ollama".to_string(),
            model: "qwen3.5".to_string(),
        }];
        let renders = render_fleet(&fleet, template, Path::new("/tmp/x.harn.prompt"), None);
        assert_eq!(renders.len(), 1);
        assert!(renders[0].error.is_none(), "{:?}", renders[0].error);
        assert!(renders[0].rendered.is_some());
    }

    #[test]
    fn line_diff_summary_reports_unique_lines() {
        let baseline = vec!["a", "b", "c"];
        let candidate = vec!["a", "b", "d"];
        let summary = line_diff_summary(&baseline, &candidate);
        assert!(summary.contains("1 line(s) only in baseline"));
        assert!(summary.contains("1 line(s) only here"));
    }

    #[test]
    fn line_diff_summary_quiet_on_identical() {
        let baseline = vec!["a", "b", "c"];
        let candidate = vec!["a", "b", "c"];
        assert_eq!(line_diff_summary(&baseline, &candidate), "");
    }
}
