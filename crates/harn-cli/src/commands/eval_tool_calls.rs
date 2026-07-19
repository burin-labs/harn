//! `harn eval tool-calls` — tool-call accuracy / latency / cost eval runner
//! plus regression-check subcommand.
//!
//! ## .harn dispatch
//!
//! The **eval pipeline** (planner / binder / judge fanout, scoring,
//! per-case streaming output) stays in Rust — every LLM call goes
//! through VM internals (`harn_vm::llm`, `llm_config`) that aren't
//! reachable from script-land today.
//!
//! Two narrow **rendering surfaces** are delegated to
//! `crates/harn-stdlib/src/stdlib/cli/eval/tool_calls.harn`:
//!
//!   1. The post-run summary line emitted after the per-case streaming
//!      lines (`tool-call eval: N/M passed (X.Y%), total_cost_usd=...`).
//!   2. The `regression-check` subcommand's verdict — both the success
//!      stdout line and the over-budget stderr failure. Reading the
//!      summary JSON files stays on the Rust side because the script
//!      runs in the `harn run` sandbox where `harness.fs.read_text` is
//!      restricted to `workspace_roots`, but `--against /tmp/foo.json`
//!      is a common invocation pattern.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use harn_vm::clock::{Clock, RealClock};
use harn_vm::llm::eval::tool_call_case::{
    load_tool_call_eval_dataset, score_tool_call_case, ExpectedToolCall, ObservedToolCall,
    ObservedToolCallOutcome, PredicateJudgeVerdict, ToolCallEvalCase, ToolCallScore,
};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::cli::{EvalToolCallsArgs, EvalToolCallsCommand, EvalToolCallsRegressionArgs};
use crate::commands::eval_model_selector::{resolve_selector, ModelSelector};
use crate::dispatch;
use crate::env_guard::ScopedEnvVar;

/// Env var the embedded `cli/eval/tool_calls` script reads to pick up
/// the rendering payload. The Rust shim assembles a small JSON
/// envelope (the shape depends on `HARN_EVAL_TOOL_CALLS_MODE`) and
/// hands it off so the script only has to format it.
const TOOL_CALLS_PAYLOAD_ENV: &str = "HARN_EVAL_TOOL_CALLS_PAYLOAD_JSON";

/// Env var the script reads to pick the rendering mode — either
/// `"summary"` (post-eval summary line) or `"regression"` (regression
/// check verdict).
const TOOL_CALLS_MODE_ENV: &str = "HARN_EVAL_TOOL_CALLS_MODE";

/// Serialises the dispatch-render path so concurrent in-process callers
/// don't race on the global env vars the Rust shim sets to hand the
/// payload off to the .harn script. Mirrors the pattern in
/// `eval_context.rs` / W5's `eval_prompt.rs` (see harn#2305 / #2306).
static DISPATCH_RENDER_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PhaseReport {
    model: ModelSelector,
    latency_ms: u64,
    input_tokens: i64,
    output_tokens: i64,
    cost_usd: f64,
    pricing_known: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    raw_response: Option<JsonValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CaseReport {
    id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tags: Vec<String>,
    expected: ExpectedToolCall,
    observed: ObservedToolCallOutcome,
    score: ToolCallScore,
    planner: PhaseReport,
    #[serde(skip_serializing_if = "Option::is_none")]
    binder: Option<PhaseReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    predicate_judge: Option<PhaseReport>,
    total_latency_ms: u64,
    total_cost_usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CaseSummary {
    id: String,
    passed: bool,
    reason: String,
    planner_latency_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    binder_latency_ms: Option<u64>,
    total_latency_ms: u64,
    cost_usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LatencyStats {
    p50_ms: u64,
    p99_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EvalSummary {
    schema_version: u32,
    dataset: PathBuf,
    output_dir: PathBuf,
    planner: ModelSelector,
    #[serde(skip_serializing_if = "Option::is_none")]
    binder: Option<ModelSelector>,
    judge_model: ModelSelector,
    total_cases: usize,
    passed_cases: usize,
    pass_rate: f64,
    total_cost_usd: f64,
    planner_latency: LatencyStats,
    #[serde(skip_serializing_if = "Option::is_none")]
    binder_latency: Option<LatencyStats>,
    total_latency: LatencyStats,
    cases: Vec<CaseSummary>,
}

#[derive(Debug, Clone, Deserialize)]
struct RegressionSummary {
    pass_rate: f64,
    #[serde(default)]
    total_cases: Option<usize>,
    #[serde(default)]
    planner: Option<ModelSelector>,
}

#[derive(Debug)]
struct RawPhaseOutput {
    response: JsonValue,
    latency_ms: u64,
}

pub async fn run(args: EvalToolCallsArgs) -> i32 {
    match args.command {
        Some(EvalToolCallsCommand::RegressionCheck(regression)) => {
            run_regression_check(regression).await
        }
        None => run_eval(args).await,
    }
}

async fn run_eval(args: EvalToolCallsArgs) -> i32 {
    let Some(planner_arg) = args.planner.as_deref() else {
        eprintln!("error: `harn eval tool-calls` requires --planner");
        return 2;
    };
    let planner = resolve_selector(planner_arg);
    let binder = args.binder.as_deref().map(resolve_selector);
    let judge_model = resolve_selector(&args.judge_model);

    let mut cases = match load_tool_call_eval_dataset(&args.dataset) {
        Ok(cases) => cases,
        Err(error) => {
            eprintln!("error: failed to load tool-call dataset: {error}");
            return 1;
        }
    };
    if let Some(filter) = args.filter.as_deref() {
        cases.retain(|case| {
            case.id.contains(filter) || case.tags.iter().any(|tag| tag.contains(filter))
        });
    }
    if let Some(max_cases) = args.max_cases {
        cases.truncate(max_cases);
    }
    if cases.is_empty() {
        eprintln!("error: no tool-call eval cases selected");
        return 1;
    }

    if args.fail_on_unauthorized
        && !all_required_provider_keys_available(&planner, binder.as_ref(), &judge_model, &cases)
    {
        return 1;
    }

    let output_dir = args.output.clone().unwrap_or_else(default_output_dir);
    if let Err(error) = fs::create_dir_all(&output_dir) {
        eprintln!("error: failed to create {}: {error}", output_dir.display());
        return 1;
    }

    let mut reports = Vec::new();
    let mut had_infra_error = false;
    for case in &cases {
        match run_case(case, &planner, binder.as_ref(), &judge_model, &args).await {
            Ok(report) => {
                eprintln!(
                    "{}: {} ({})",
                    case.id,
                    if report.score.passed { "pass" } else { "fail" },
                    report.score.reason
                );
                reports.push(report);
            }
            Err(error) => {
                had_infra_error = true;
                eprintln!("{}: error: {error}", case.id);
            }
        }
    }

    let summary = build_summary(
        &args.dataset,
        &output_dir,
        planner,
        binder,
        judge_model,
        &reports,
    );
    if let Err(error) = write_outputs(&output_dir, &summary, &reports) {
        eprintln!("error: failed to write eval outputs: {error}");
        return 1;
    }
    eprintln!(
        "wrote {} and {}",
        output_dir.join("summary.json").display(),
        output_dir.join("per_case.jsonl").display()
    );

    let summary_line = match dispatch_summary_line(&summary).await {
        Ok(line) => line,
        Err(code) => return code,
    };
    println!("{summary_line}");

    i32::from(had_infra_error)
}

#[derive(Debug, Serialize)]
struct SummaryEnvelope {
    passed_cases: usize,
    total_cases: usize,
    pass_rate: f64,
    total_cost_usd: f64,
}

async fn dispatch_summary_line(summary: &EvalSummary) -> Result<String, i32> {
    let envelope = SummaryEnvelope {
        passed_cases: summary.passed_cases,
        total_cases: summary.total_cases,
        pass_rate: summary.pass_rate,
        total_cost_usd: summary.total_cost_usd,
    };
    let payload_json = match serde_json::to_string(&envelope) {
        Ok(json) => json,
        Err(error) => {
            eprintln!("error: failed to serialise tool-calls summary envelope: {error}");
            return Err(1);
        }
    };
    let _guard = DISPATCH_RENDER_LOCK.lock().await;
    let _payload = ScopedEnvVar::set(TOOL_CALLS_PAYLOAD_ENV, &payload_json);
    let _mode = ScopedEnvVar::set(TOOL_CALLS_MODE_ENV, "summary");
    let outcome = dispatch::run_embedded_script("eval/tool_calls", Vec::new(), false).await;
    if !outcome.stderr.is_empty() {
        let _ = std::io::stderr().write_all(outcome.stderr.as_bytes());
    }
    if outcome.exit_code != 0 {
        return Err(outcome.exit_code);
    }
    Ok(outcome.stdout.trim_end_matches('\n').to_string())
}

async fn run_case(
    case: &ToolCallEvalCase,
    planner: &ModelSelector,
    binder: Option<&ModelSelector>,
    judge_model: &ModelSelector,
    args: &EvalToolCallsArgs,
) -> Result<CaseReport, String> {
    let planner_output = execute_harn_json(&planner_script(
        case,
        planner,
        args.tool_format.as_deref(),
        args.max_tokens,
    ))
    .await?;
    let planner_response = planner_output.response.clone();
    let planner_phase = phase_report(planner.clone(), planner_output);

    let (observed, binder_phase) = if let Some(binder) = binder {
        let binder_output = execute_harn_json(&binder_script(
            case,
            &planner_response,
            binder,
            args.binder_max_tokens,
        ))
        .await?;
        let binder_response = binder_output.response.clone();
        let binder_phase = phase_report(binder.clone(), binder_output);
        (observed_from_binder(&binder_response), Some(binder_phase))
    } else {
        (observed_from_llm_response(&planner_response), None)
    };

    let predicate_judge = if matches!(case.expected, ExpectedToolCall::Predicate { .. }) {
        let judge_output = execute_harn_json(&predicate_judge_script(
            case,
            &observed,
            judge_model,
            args.binder_max_tokens,
        ))
        .await?;
        Some(phase_report(judge_model.clone(), judge_output))
    } else {
        None
    };
    let predicate_verdict = predicate_judge
        .as_ref()
        .and_then(|phase| phase.raw_response.as_ref())
        .and_then(predicate_verdict_from_response);

    let score = score_tool_call_case(case, &observed, predicate_verdict.as_ref());
    let total_latency_ms = planner_phase.latency_ms
        + binder_phase
            .as_ref()
            .map(|phase| phase.latency_ms)
            .unwrap_or(0)
        + predicate_judge
            .as_ref()
            .map(|phase| phase.latency_ms)
            .unwrap_or(0);
    let total_cost_usd = planner_phase.cost_usd
        + binder_phase
            .as_ref()
            .map(|phase| phase.cost_usd)
            .unwrap_or(0.0)
        + predicate_judge
            .as_ref()
            .map(|phase| phase.cost_usd)
            .unwrap_or(0.0);
    Ok(CaseReport {
        id: case.id.clone(),
        tags: case.tags.clone(),
        expected: case.expected.clone(),
        observed,
        score,
        planner: planner_phase,
        binder: binder_phase,
        predicate_judge,
        total_latency_ms,
        total_cost_usd,
    })
}

fn all_required_provider_keys_available(
    planner: &ModelSelector,
    binder: Option<&ModelSelector>,
    judge_model: &ModelSelector,
    cases: &[ToolCallEvalCase],
) -> bool {
    let mut selectors = vec![planner];
    if let Some(binder) = binder {
        selectors.push(binder);
    }
    if cases
        .iter()
        .any(|case| matches!(case.expected, ExpectedToolCall::Predicate { .. }))
    {
        selectors.push(judge_model);
    }

    for selector in selectors {
        if selector.provider != "mock"
            && selector.provider != "fake"
            && !harn_vm::llm::provider_auth_status(&selector.provider).available
        {
            eprintln!(
                "error: provider `{}` for `{}` has no configured credentials",
                selector.provider, selector.selector
            );
            return false;
        }
    }
    true
}

fn phase_report(model: ModelSelector, output: RawPhaseOutput) -> PhaseReport {
    let provider = output
        .response
        .get("provider")
        .and_then(JsonValue::as_str)
        .unwrap_or(&model.provider);
    let model_id = output
        .response
        .get("model")
        .and_then(JsonValue::as_str)
        .unwrap_or(&model.model);
    let input_tokens = token_field(&output.response, "input_tokens");
    let output_tokens = token_field(&output.response, "output_tokens");
    let pricing = harn_vm::llm::llm_pricing_per_1k(provider, model_id);
    let cost_usd = pricing
        .map(|(input, output)| {
            (input_tokens.max(0) as f64 * input + output_tokens.max(0) as f64 * output) / 1000.0
        })
        .unwrap_or(0.0);
    PhaseReport {
        model,
        latency_ms: output.latency_ms,
        input_tokens,
        output_tokens,
        cost_usd,
        pricing_known: pricing.is_some(),
        raw_response: Some(output.response),
    }
}

fn token_field(response: &JsonValue, key: &str) -> i64 {
    response
        .get(key)
        .and_then(JsonValue::as_i64)
        .or_else(|| {
            response
                .get("usage")
                .and_then(|usage| usage.get(key))
                .and_then(JsonValue::as_i64)
        })
        .unwrap_or(0)
}

fn observed_from_llm_response(response: &JsonValue) -> ObservedToolCallOutcome {
    let call = response
        .get("tool_calls")
        .and_then(JsonValue::as_array)
        .and_then(|calls| calls.first())
        .and_then(observed_tool_call_from_value);
    ObservedToolCallOutcome {
        tool_call: call,
        final_text: response_text(response),
    }
}

/// Decode the binder's `arguments_json` (string) into a JSON object. Falls
/// back to the legacy `arguments` (object) field — older or non-strict
/// providers may emit the inline object form, and accepting both keeps the
/// scorer agnostic to which binder shape produced the verdict.
fn parse_binder_arguments(data: &JsonValue) -> JsonValue {
    if let Some(text) = data.get("arguments_json").and_then(JsonValue::as_str) {
        if let Ok(parsed) = serde_json::from_str::<JsonValue>(text) {
            if parsed.is_object() {
                return parsed;
            }
        }
    }
    data.get("arguments")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}))
}

fn observed_from_binder(response: &JsonValue) -> ObservedToolCallOutcome {
    let data = response
        .get("data")
        .cloned()
        .or_else(|| {
            response
                .get("text")
                .and_then(JsonValue::as_str)
                .and_then(|text| serde_json::from_str::<JsonValue>(text).ok())
        })
        .unwrap_or(JsonValue::Null);
    let decision = data
        .get("decision")
        .and_then(JsonValue::as_str)
        .unwrap_or_default();
    if decision == "call" {
        let name = data
            .get("name")
            .and_then(JsonValue::as_str)
            .unwrap_or_default()
            .to_string();
        let args = parse_binder_arguments(&data);
        return ObservedToolCallOutcome {
            tool_call: (!name.is_empty()).then_some(ObservedToolCall { name, args }),
            final_text: response_text(response),
        };
    }
    ObservedToolCallOutcome {
        tool_call: None,
        final_text: data
            .get("reason")
            .and_then(JsonValue::as_str)
            .map(ToString::to_string)
            .unwrap_or_else(|| response_text(response)),
    }
}

fn observed_tool_call_from_value(value: &JsonValue) -> Option<ObservedToolCall> {
    let name = value
        .get("name")
        .or_else(|| {
            value
                .get("function")
                .and_then(|function| function.get("name"))
        })
        .and_then(JsonValue::as_str)?
        .to_string();
    let args = value
        .get("arguments")
        .or_else(|| value.get("args"))
        .cloned()
        .or_else(|| {
            value
                .get("function")
                .and_then(|function| function.get("arguments"))
                .cloned()
        })
        .and_then(parse_argument_value)
        .unwrap_or_else(|| serde_json::json!({}));
    Some(ObservedToolCall { name, args })
}

fn parse_argument_value(value: JsonValue) -> Option<JsonValue> {
    match value {
        JsonValue::String(text) => serde_json::from_str(&text).ok(),
        other => Some(other),
    }
}

fn response_text(response: &JsonValue) -> String {
    response
        .get("text")
        .and_then(JsonValue::as_str)
        .unwrap_or_default()
        .to_string()
}

fn predicate_verdict_from_response(response: &JsonValue) -> Option<PredicateJudgeVerdict> {
    let data = response.get("data").cloned().or_else(|| {
        response
            .get("text")
            .and_then(JsonValue::as_str)
            .and_then(|text| serde_json::from_str::<JsonValue>(text).ok())
    })?;
    Some(PredicateJudgeVerdict {
        passed: data
            .get("passed")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false),
        reason: data
            .get("reason")
            .and_then(JsonValue::as_str)
            .unwrap_or_default()
            .to_string(),
    })
}

async fn execute_harn_json(script: &str) -> Result<RawPhaseOutput, String> {
    let tmp = tempfile::Builder::new()
        .prefix("harn-tool-call-eval-")
        .suffix(".harn")
        .tempfile()
        .map_err(|error| format!("tempfile: {error}"))?;
    fs::write(tmp.path(), script).map_err(|error| format!("write tempfile: {error}"))?;

    let clock = RealClock::new();
    let started_ms = clock.monotonic_ms();
    let outcome = crate::commands::run::execute_run(
        &tmp.path().to_string_lossy(),
        false,
        std::collections::HashSet::new(),
        Vec::new(),
        Vec::new(),
        crate::commands::run::CliLlmMockMode::Off,
        None,
        crate::commands::run::RunProfileOptions::default(),
    )
    .await;
    let latency_ms = clock
        .monotonic_ms()
        .saturating_sub(started_ms)
        .try_into()
        .unwrap_or(0);
    if outcome.exit_code != 0 {
        return Err(format!(
            "harn run exited {}: {}",
            outcome.exit_code,
            outcome.stderr.trim()
        ));
    }
    for line in outcome.stdout.lines().rev() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(response) = serde_json::from_str::<JsonValue>(trimmed) {
            return Ok(RawPhaseOutput {
                response,
                latency_ms,
            });
        }
    }
    Err("harn script produced no JSON response".to_string())
}

fn planner_script(
    case: &ToolCallEvalCase,
    planner: &ModelSelector,
    tool_format: Option<&str>,
    max_tokens: i64,
) -> String {
    let tools_lit = json_string_literal(&serde_json::to_string(&case.tools).unwrap());
    let prompt_lit = json_string_literal(&case.prompt);
    let provider_lit = json_string_literal(&planner.provider);
    let model_lit = json_string_literal(&planner.model);
    let tool_format_line = tool_format
        .map(|format| format!("      tool_format: {},\n", json_string_literal(format)))
        .unwrap_or_default();
    // Deliberately no planner system prompt. The PEAR-style eval is meant
    // to measure a *weak* planner against the binder's lift — adding a
    // curated system prompt strengthens the planner enough that some
    // tests measure the wrong thing (literal extraction wins on some
    // cases, hurts on others where the schema field implies a canonical
    // form like "CA" for `country_code` or "errors" for the user's
    // explicit plural). See discussion on #1698.
    format!(
        "pipeline main() {{\n\
  const tools = json_parse({tools_lit})\n\
  const response = llm_call(\n\
    {prompt_lit},\n\
    nil,\n\
    {{\n\
      provider: {provider_lit},\n\
      model: {model_lit},\n\
      tools: tools,\n\
{tool_format_line}\
      max_tokens: {max_tokens}\n\
    }},\n\
  )\n\
  __io_println(json_stringify(response))\n\
}}\n"
    )
}

fn binder_script(
    case: &ToolCallEvalCase,
    planner_response: &JsonValue,
    binder: &ModelSelector,
    max_tokens: i64,
) -> String {
    let prompt = binder_prompt(case, planner_response);
    let prompt_lit = json_string_literal(&prompt);
    let schema_lit = json_string_literal(&serde_json::to_string(&binder_schema()).unwrap());
    let provider_lit = json_string_literal(&binder.provider);
    let model_lit = json_string_literal(&binder.model);
    format!(
        "pipeline main() {{\n\
  const schema = json_parse({schema_lit})\n\
  const response = llm_call(\n\
    {prompt_lit},\n\
    nil,\n\
    {{\n\
      provider: {provider_lit},\n\
      model: {model_lit},\n\
      output: {{schema: schema, strict: true, validation: \"warn\"}},\n\
      max_tokens: {max_tokens}\n\
    }},\n\
  )\n\
  __io_println(json_stringify(response))\n\
}}\n"
    )
}

fn predicate_judge_script(
    case: &ToolCallEvalCase,
    observed: &ObservedToolCallOutcome,
    judge: &ModelSelector,
    max_tokens: i64,
) -> String {
    let prompt = predicate_judge_prompt(case, observed);
    let prompt_lit = json_string_literal(&prompt);
    let schema_lit =
        json_string_literal(&serde_json::to_string(&predicate_judge_schema()).unwrap());
    let provider_lit = json_string_literal(&judge.provider);
    let model_lit = json_string_literal(&judge.model);
    format!(
        "pipeline main() {{\n\
  const schema = json_parse({schema_lit})\n\
  const response = llm_call(\n\
    {prompt_lit},\n\
    nil,\n\
    {{\n\
      provider: {provider_lit},\n\
      model: {model_lit},\n\
      output: {{schema: schema, strict: true, validation: \"warn\"}},\n\
      max_tokens: {max_tokens}\n\
    }},\n\
  )\n\
  __io_println(json_stringify(response))\n\
}}\n"
    )
}

fn binder_prompt(case: &ToolCallEvalCase, planner_response: &JsonValue) -> String {
    let tools = serde_json::to_string_pretty(&case.tools).unwrap_or_default();
    let planner = serde_json::to_string_pretty(planner_response).unwrap_or_default();
    // Authority is the user's request + the tool schemas. The planner's
    // response is a hint to evaluate, not a transcript to canonicalize.
    // The binder's job is to produce the MINIMUM CORRECT tool call —
    // strip articles, strip user-supplied quotes around literals, strip
    // elaborations ("uses of X" → "X"), drop default args the user
    // didn't request, coerce types to match the schema. The planner's
    // args are useful as a starting hypothesis but should be overridden
    // freely when needed. The two few-shot examples below pin down the
    // failure modes the v1/v2 prompts regressed on: literal-quote
    // insertion and surface-phrase echo.
    format!(
        "You are a fast schema-binder. The user's request and the declared tool schemas are \
the source of truth; the planner's response is a hint to evaluate, not a transcript to \
copy.\n\
\n\
Produce the MINIMUM correct tool call:\n\
- pick exactly one declared tool, or refuse;\n\
- argument values should be the SHORTEST faithful extraction of the user's actual target:\n\
  - drop leading articles (\"the\", \"a\", \"an\") unless they're part of a proper noun;\n\
  - strip the user's action verb (\"uses of X\" → \"X\"; \"information about Y\" → \"Y\"; \
\"X plan\" → \"X\" when the user's intent is X itself, not the plan);\n\
  - if the user wrapped a literal in single or double quotes, strip the quotes;\n\
  - omit optional arguments the user did not request, even if they have schema defaults;\n\
- JSON types must EXACTLY match the schema: numbers as JSON numbers (42, not \"42\"), \
booleans as true/false, arrays as arrays. The user saying \"id is 42\" means an integer.\n\
\n\
Refuse (decision=refusal) ONLY when:\n\
- no declared tool can serve the user's request, OR\n\
- a required argument is genuinely missing and unrecoverable from the user's request, OR\n\
- the request is purely conversational chitchat.\n\
Refusal reason text should include the word \"no\" or \"not\" or \"cannot\", plus the \
capability the user wanted (e.g. \"no tool for refunds\", \"cannot translate\", \"no order \
id provided in the request\").\n\
\n\
Output JSON with these fields (all required):\n\
- decision: \"call\" or \"refusal\"\n\
- name: declared tool name when decision=call, else \"\".\n\
- arguments_json: when decision=call, JSON-stringified args object; when refusal, \"{{}}\".\n\
- reason: one short line.\n\
\n\
Example 1 — strip elaboration, type coercion:\n\
  User prompt: Select email from users where id is 42.\n\
  Correct output: {{\"decision\":\"call\",\"name\":\"sql_execute\",\
\"arguments_json\":\"{{\\\"sql_keyword\\\":\\\"SELECT\\\",\\\"table_name\\\":\\\"users\\\",\
\\\"columns\\\":[\\\"email\\\"],\\\"conditions\\\":{{\\\"id\\\":42}}}}\",\"reason\":\"call\"}}\n\
  WRONG: id=\"42\" (the user said \"42\" as a number, not a string).\n\
\n\
Example 2 — strip user-supplied quotes around literals:\n\
  User prompt: Translate 'good morning' to Spanish.\n\
  Correct output: {{\"decision\":\"call\",\"name\":\"translate_text\",\
\"arguments_json\":\"{{\\\"text\\\":\\\"good morning\\\",\\\"target_language\\\":\\\"Spanish\\\"}}\",\"reason\":\"call\"}}\n\
  WRONG: text=\"'good morning'\" (the single quotes around \"good morning\" are part of \
the user's notation, not part of the literal text to translate).\n\
\n\
Example 3 — strip elaboration, plural preserved:\n\
  User prompt: Search the repository for uses of llm_call.\n\
  Correct output: {{\"decision\":\"call\",\"name\":\"repo_search\",\
\"arguments_json\":\"{{\\\"query\\\":\\\"llm_call\\\"}}\",\"reason\":\"call\"}}\n\
  WRONG: query=\"uses of llm_call\" (the action verb \"uses of\" is the user's framing, \
not part of the search target).\n\
\n\
User prompt:\n{}\n\nDeclared tools:\n{}\n\nPlanner hint:\n{}",
        case.prompt, tools, planner
    )
}

fn predicate_judge_prompt(case: &ToolCallEvalCase, observed: &ObservedToolCallOutcome) -> String {
    let ExpectedToolCall::Predicate {
        description,
        judge_prompt,
    } = &case.expected
    else {
        return String::new();
    };
    let observed = serde_json::to_string_pretty(observed).unwrap_or_default();
    format!(
        "{judge_prompt}\n\nRubric:\n{description}\n\nObserved tool decision:\n{observed}\n\nReturn JSON with passed and reason."
    )
}

fn binder_schema() -> JsonValue {
    // `arguments` is a tool-specific object whose shape varies per case, so
    // we cannot pre-declare its `properties`. Encode it as a JSON-stringified
    // payload instead: providers like Cerebras enforce strict structured
    // output and reject `{"type": "object"}` without a properties list. The
    // scorer parses the string back via `observed_from_binder`.
    //
    // All fields are unconditionally required — strict OpenAI-compat
    // providers (Cerebras, Together) reject conditional `if/then/else`
    // requirements at the schema layer, so we surface the convention in
    // the prompt instead: refusals emit empty strings for `name` and
    // `arguments_json` (`"{}"`).
    serde_json::json!({
        "type": "object",
        "required": ["decision", "name", "arguments_json", "reason"],
        "properties": {
            "decision": {"type": "string", "enum": ["call", "refusal"]},
            "name": {"type": "string"},
            "arguments_json": {"type": "string"},
            "reason": {"type": "string"}
        },
        "additionalProperties": false
    })
}

fn predicate_judge_schema() -> JsonValue {
    serde_json::json!({
        "type": "object",
        "required": ["passed", "reason"],
        "properties": {
            "passed": {"type": "boolean"},
            "reason": {"type": "string"}
        },
        "additionalProperties": false
    })
}

fn json_string_literal(value: &str) -> String {
    JsonValue::String(value.to_string()).to_string()
}

fn build_summary(
    dataset: &Path,
    output_dir: &Path,
    planner: ModelSelector,
    binder: Option<ModelSelector>,
    judge_model: ModelSelector,
    reports: &[CaseReport],
) -> EvalSummary {
    let passed_cases = reports.iter().filter(|report| report.score.passed).count();
    let cases = reports
        .iter()
        .map(|report| CaseSummary {
            id: report.id.clone(),
            passed: report.score.passed,
            reason: report.score.reason.clone(),
            planner_latency_ms: report.planner.latency_ms,
            binder_latency_ms: report.binder.as_ref().map(|phase| phase.latency_ms),
            total_latency_ms: report.total_latency_ms,
            cost_usd: report.total_cost_usd,
        })
        .collect();
    let binder_latencies = reports
        .iter()
        .filter_map(|report| report.binder.as_ref().map(|phase| phase.latency_ms))
        .collect();
    EvalSummary {
        schema_version: 1,
        dataset: dataset.to_path_buf(),
        output_dir: output_dir.to_path_buf(),
        planner,
        binder,
        judge_model,
        total_cases: reports.len(),
        passed_cases,
        pass_rate: if reports.is_empty() {
            0.0
        } else {
            passed_cases as f64 / reports.len() as f64
        },
        total_cost_usd: reports.iter().map(|report| report.total_cost_usd).sum(),
        planner_latency: latency_stats(reports.iter().map(|report| report.planner.latency_ms)),
        binder_latency: latency_stats_option(binder_latencies),
        total_latency: latency_stats(reports.iter().map(|report| report.total_latency_ms)),
        cases,
    }
}

fn latency_stats_option(values: Vec<u64>) -> Option<LatencyStats> {
    (!values.is_empty()).then(|| latency_stats(values))
}

fn latency_stats(values: impl IntoIterator<Item = u64>) -> LatencyStats {
    let mut values: Vec<u64> = values.into_iter().collect();
    if values.is_empty() {
        return LatencyStats {
            p50_ms: 0,
            p99_ms: 0,
        };
    }
    values.sort_unstable();
    LatencyStats {
        p50_ms: super::nearest_rank_percentile(&values, 0.50).unwrap_or(0),
        p99_ms: super::nearest_rank_percentile(&values, 0.99).unwrap_or(0),
    }
}

fn write_outputs(
    output_dir: &Path,
    summary: &EvalSummary,
    reports: &[CaseReport],
) -> Result<(), String> {
    fs::write(
        output_dir.join("summary.json"),
        serde_json::to_string_pretty(summary).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let mut jsonl = String::new();
    for report in reports {
        jsonl.push_str(&serde_json::to_string(report).map_err(|error| error.to_string())?);
        jsonl.push('\n');
    }
    fs::write(output_dir.join("per_case.jsonl"), jsonl).map_err(|error| error.to_string())
}

fn default_output_dir() -> PathBuf {
    let now = RealClock::new().now_utc().unix_timestamp().max(0);
    PathBuf::from(".harn-runs")
        .join("tool-call-eval")
        .join(now.to_string())
}

async fn run_regression_check(args: EvalToolCallsRegressionArgs) -> i32 {
    let current_path = args
        .current
        .clone()
        .unwrap_or_else(|| PathBuf::from(".harn-runs/tool-call-eval/latest/summary.json"));
    let current = match read_regression_summary(&current_path) {
        Ok(summary) => summary,
        Err(error) => {
            eprintln!("error: failed to read current summary: {error}");
            return 1;
        }
    };
    let baseline = match read_regression_summary(&args.against) {
        Ok(summary) => summary,
        Err(error) => {
            eprintln!("error: failed to read baseline summary: {error}");
            return 1;
        }
    };
    let label = args
        .planner
        .as_deref()
        .or_else(|| {
            current
                .planner
                .as_ref()
                .map(|planner| planner.selector.as_str())
        })
        .unwrap_or("current")
        .to_string();

    dispatch_regression_render(&current, &baseline, &label, args.max_drop_pp).await
}

#[derive(Debug, Serialize)]
struct RegressionEnvelope<'a> {
    current_pass_rate: f64,
    baseline_pass_rate: f64,
    max_drop_pp: f64,
    label: &'a str,
    total_cases_mismatch: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    current_total_cases: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    baseline_total_cases: Option<usize>,
}

async fn dispatch_regression_render(
    current: &RegressionSummary,
    baseline: &RegressionSummary,
    label: &str,
    max_drop_pp: f64,
) -> i32 {
    let total_cases_mismatch = matches!(
        (current.total_cases, baseline.total_cases),
        (Some(c), Some(b)) if c != b
    );
    let envelope = RegressionEnvelope {
        current_pass_rate: current.pass_rate,
        baseline_pass_rate: baseline.pass_rate,
        max_drop_pp,
        label,
        total_cases_mismatch,
        current_total_cases: current.total_cases,
        baseline_total_cases: baseline.total_cases,
    };
    let payload_json = match serde_json::to_string(&envelope) {
        Ok(json) => json,
        Err(error) => {
            eprintln!("error: failed to serialise regression envelope: {error}");
            return 1;
        }
    };
    let _guard = DISPATCH_RENDER_LOCK.lock().await;
    let _payload = ScopedEnvVar::set(TOOL_CALLS_PAYLOAD_ENV, &payload_json);
    let _mode = ScopedEnvVar::set(TOOL_CALLS_MODE_ENV, "regression");
    let outcome = dispatch::run_embedded_script("eval/tool_calls", Vec::new(), false).await;
    if !outcome.stderr.is_empty() {
        let _ = std::io::stderr().write_all(outcome.stderr.as_bytes());
    }
    if !outcome.stdout.is_empty() {
        // The script's stdout already terminates with a newline (via
        // harness.stdio.println), so forward it verbatim.
        let _ = std::io::stdout().write_all(outcome.stdout.as_bytes());
    }
    outcome.exit_code
}

fn read_regression_summary(path: &Path) -> Result<RegressionSummary, String> {
    let raw = fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
    serde_json::from_str(&raw).map_err(|error| format!("{}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn exact_eval_case() -> ToolCallEvalCase {
        ToolCallEvalCase {
            id: "exact".to_string(),
            prompt: "Search Harn docs".to_string(),
            tools: vec![harn_vm::llm::eval::tool_call_case::ToolDef {
                name: "search".to_string(),
                description: String::new(),
                parameters: json!({"query": {"type": "string"}}),
                output_schema: None,
                namespace: None,
                defer_loading: None,
            }],
            expected: ExpectedToolCall::Exact {
                name: "search".to_string(),
                args: json!({"query": "Harn docs"}),
            },
            baseline_pass_rate: None,
            source: None,
            tags: Vec::new(),
        }
    }

    #[test]
    fn selector_accepts_key_value_and_colon_forms() {
        let kv = resolve_selector("provider=openrouter,model=google/gemma");
        assert_eq!(kv.provider, "openrouter");
        assert_eq!(kv.model, "google/gemma");

        let colon = resolve_selector("mock:mock");
        assert_eq!(colon.provider, "mock");
        assert_eq!(colon.model, "mock");
    }

    #[test]
    fn observed_from_native_tool_response_uses_first_call() {
        let observed = observed_from_llm_response(&json!({
            "tool_calls": [{"name": "search", "arguments": {"query": "harn"}}],
            "text": ""
        }));
        assert_eq!(observed.tool_call.unwrap().name, "search");
    }

    #[test]
    fn observed_from_binder_handles_refusals() {
        let observed = observed_from_binder(&json!({
            "data": {"decision": "refusal", "reason": "no matching tool"}
        }));
        assert!(observed.tool_call.is_none());
        assert_eq!(observed.final_text, "no matching tool");
    }

    #[test]
    fn observed_from_binder_parses_arguments_json_string() {
        let observed = observed_from_binder(&json!({
            "data": {
                "decision": "call",
                "name": "search",
                "arguments_json": "{\"query\":\"harn\"}",
                "reason": "ok",
            },
            "text": "",
        }));
        let call = observed.tool_call.expect("binder call");
        assert_eq!(call.name, "search");
        assert_eq!(call.args, json!({"query": "harn"}));
    }

    #[test]
    fn observed_from_binder_falls_back_to_inline_arguments_object() {
        let observed = observed_from_binder(&json!({
            "data": {
                "decision": "call",
                "name": "search",
                "arguments": {"query": "harn"},
                "reason": "ok",
            },
            "text": "",
        }));
        let call = observed.tool_call.expect("binder call");
        assert_eq!(call.args, json!({"query": "harn"}));
    }

    #[test]
    fn provider_key_check_skips_unused_predicate_judge() {
        let planner = resolve_selector("mock:mock");
        let judge = resolve_selector("provider=definitely_missing,model=judge");
        assert!(all_required_provider_keys_available(
            &planner,
            None,
            &judge,
            &[exact_eval_case()]
        ));
    }

    #[test]
    fn regression_summary_accepts_minimal_baseline() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        fs::write(
            tmp.path(),
            r#"{"pass_rate":0.82,"total_cases":50,"planner":{"selector":"mock:mock","provider":"mock","model":"mock"}}"#,
        )
        .unwrap();

        let summary = read_regression_summary(tmp.path()).unwrap();
        assert_eq!(summary.pass_rate, 0.82);
        assert_eq!(summary.total_cases, Some(50));
        assert_eq!(summary.planner.unwrap().selector, "mock:mock");
    }
}
