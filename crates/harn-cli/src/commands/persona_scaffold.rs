use std::fs;
use std::path::{Path, PathBuf};

use crate::cli::{PersonaNewArgs, PersonaTemplateKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonaScaffoldResult {
    pub root: PathBuf,
    pub files: Vec<PathBuf>,
}

#[derive(Clone, Copy)]
struct TemplateFile {
    relative_path: &'static str,
    content: &'static str,
}

const TEMPLATE_FILES: &[TemplateFile] = &[
    TemplateFile {
        relative_path: "harn.toml",
        content: HARN_TOML_TEMPLATE,
    },
    TemplateFile {
        relative_path: "src/{{persona_name}}.harn",
        content: SOURCE_TEMPLATE,
    },
    TemplateFile {
        relative_path: "tests/{{persona_name}}_smoke.harn",
        content: SMOKE_TEST_TEMPLATE,
    },
    TemplateFile {
        relative_path: "tests/{{persona_name}}_smoke.expected",
        content: SMOKE_EXPECTED_TEMPLATE,
    },
    TemplateFile {
        relative_path: "fixtures/happy_path.json",
        content: FIXTURE_TEMPLATE,
    },
    TemplateFile {
        relative_path: "prompts/system.harn.prompt",
        content: PROMPT_TEMPLATE,
    },
    TemplateFile {
        relative_path: "evals/smoke.eval.json",
        content: EVAL_TEMPLATE,
    },
    TemplateFile {
        relative_path: "README.md",
        content: README_TEMPLATE,
    },
];

const HARN_TOML_TEMPLATE: &str = r#"[package]
name = "harn-{{package_slug}}-persona"
version = "0.1.0"
description = "{{persona_title}} persona scaffold."

[[personas]]
name = "{{persona_name}}"
version = "0.1.0"
description = "{{persona_title}} persona scaffold generated from the {{template_kind}} template."
entry_workflow = "src/{{persona_name}}.harn#run"
tools = {{tools}}
capabilities = {{capabilities}}
autonomy_tier = "{{autonomy_tier}}"
receipt_policy = "required"
triggers = {{triggers}}
schedules = []
handoffs = []
context_packs = ["repo_policy"]
evals = ["smoke"]
owner = "platform"
budget = {{budget}}
model_policy = {{model_policy}}
rollout_policy = { mode = "approval_only", percentage = 100, cohorts = ["scaffold"] }
package_source = { package = "harn-{{package_slug}}-persona", path = "." }
"#;

const SOURCE_TEMPLATE: &str = r#"import { bounded_loop, cheap_classify_then_escalate, with_audit_receipt } from "std/personas/prelude"

@persona(
  name: "{{persona_name}}",
  description: "{{persona_title}} persona scaffold generated from the {{template_kind}} template.",
  tools: {{tools_symbols}},
  capabilities: {{capabilities}},
  autonomy: "{{autonomy_tier}}",
  receipts: "required",
)
/** Run the scaffolded persona DAG. */
pub fn {{persona_ident}}(task) {
{{persona_body}}
}

{{steps}}

pipeline run(task) {
  return {{persona_ident}}(task)
}
"#;

const SMOKE_TEST_TEMPLATE: &str = r#"import { {{persona_ident}} } from "../src/{{persona_name}}"

pipeline test_{{persona_ident}}_smoke(task) {
  llm_mock_clear()
{{llm_mocks}}
  let fixture = json_parse(read_file("../fixtures/happy_path.json"))
  let result = {{persona_ident}}({
    fixture: fixture,
    started_at: "2026-05-05T00:00:00Z",
    completed_at: "2026-05-05T00:00:00Z",
  })
  assert_eq(result.ok, true)
  assert_eq(result.receipt.schema, "harn.receipt.v1")
{{smoke_assertions}}
  log(result.ok)
  log(result.receipt.schema)
}
"#;

const SMOKE_EXPECTED_TEMPLATE: &str = r#"[harn] true
[harn] harn.receipt.v1
"#;

const FIXTURE_TEMPLATE: &str = r#"{
  "items": [
    {"id": "demo-1", "title": "First scaffolded work item", "risk": "low"},
    {"id": "demo-2", "title": "Second scaffolded work item", "risk": "medium"}
  ],
  "request": "Decide the next safe action for this persona package."
}
"#;

const PROMPT_TEMPLATE: &str = r#"You are {{persona_title}}, a Harn persona.

Use the fixture request and typed step metadata to produce a bounded, auditable decision.

Persona: {{persona_name}}
Template: {{template_kind}}
Request: {{request}}
"#;

const EVAL_TEMPLATE: &str = r#"{
  "_type": "eval_pack_manifest",
  "id": "{{persona_name}}-smoke",
  "persona": "{{persona_name}}",
  "fixtures": ["fixtures/happy_path.json"],
  "metrics": [
    {"name": "smoke_passed", "kind": "boolean"}
  ]
}
"#;

const README_TEMPLATE: &str = r#"# {{persona_title}}

Generated from the `{{template_kind}}` persona template.

## Validate

```bash
harn persona doctor {{persona_name}}
```

## Run The Smoke Test

```bash
harn test tests/{{persona_name}}_smoke.harn
```

## Package Layout

- `harn.toml` declares the durable persona manifest and budget/model defaults.
- `src/{{persona_name}}.harn` contains the `@persona` function and typed `@step` DAG skeleton.
- `prompts/system.harn.prompt` keeps model-facing instructions as a prompt asset.
- `fixtures/happy_path.json`, `tests/{{persona_name}}_smoke.harn`, and `tests/{{persona_name}}_smoke.expected` form the first eval-ready fixture pair.
"#;

pub(crate) fn scaffold_persona(args: &PersonaNewArgs) -> Result<PersonaScaffoldResult, String> {
    let name = normalize_name(&args.name)?;
    let target_root = args.output_root.join(&name);
    if target_root.exists() {
        if !args.force {
            return Err(format!(
                "{} already exists; pass --force to replace generated files",
                target_root.display()
            ));
        }
        fs::remove_dir_all(&target_root).map_err(|error| {
            format!(
                "failed to remove existing persona package {}: {error}",
                target_root.display()
            )
        })?;
    }

    let vars = TemplateVars::new(&name, args.template);
    let mut files = Vec::new();
    for template in TEMPLATE_FILES {
        let relative = render(template.relative_path, &vars);
        let path = target_root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
        }
        let content = render(template.content, &vars);
        fs::write(&path, content)
            .map_err(|error| format!("failed to write {}: {error}", path.display()))?;
        files.push(path);
    }

    Ok(PersonaScaffoldResult {
        root: target_root,
        files,
    })
}

pub fn scaffold_persona_package(
    name: &str,
    template: &str,
    output_root: &Path,
    force: bool,
) -> Result<PersonaScaffoldResult, String> {
    let args = PersonaNewArgs {
        name: name.to_string(),
        template: parse_template_kind(template)?,
        output_root: output_root.to_path_buf(),
        force,
    };
    scaffold_persona(&args)
}

pub(crate) fn run_new(args: &PersonaNewArgs) -> Result<(), String> {
    let result = scaffold_persona(args)?;
    println!("created persona package {}", result.root.display());
    for file in &result.files {
        println!("  create {}", file.display());
    }
    println!();
    println!("  harn persona doctor {}", normalize_name(&args.name)?);
    Ok(())
}

fn normalize_name(raw: &str) -> Result<String, String> {
    let name = raw.trim().replace('-', "_");
    if !valid_identifier(&name) {
        return Err(format!(
            "persona name must be an identifier-like token, got {raw:?}"
        ));
    }
    Ok(name)
}

fn valid_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    match chars.next() {
        Some(ch) if ch == '_' || ch.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn parse_template_kind(value: &str) -> Result<PersonaTemplateKind, String> {
    match value {
        "deterministic-sweeper" => Ok(PersonaTemplateKind::DeterministicSweeper),
        "hybrid-classify-then-act" => Ok(PersonaTemplateKind::HybridClassifyThenAct),
        "frontier-judgment-loop" => Ok(PersonaTemplateKind::FrontierJudgmentLoop),
        _ => Err(format!("unknown persona template {value:?}")),
    }
}

fn to_title(name: &str) -> String {
    name.split('_')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => format!("{}{}", first.to_ascii_uppercase(), chars.as_str()),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn to_package_slug(name: &str) -> String {
    name.replace('_', "-")
}

struct TemplateVars {
    persona_name: String,
    persona_ident: String,
    persona_title: String,
    package_slug: String,
    template_kind: &'static str,
    tools: &'static str,
    tools_symbols: &'static str,
    capabilities: &'static str,
    autonomy_tier: &'static str,
    triggers: &'static str,
    budget: &'static str,
    model_policy: &'static str,
    persona_body: &'static str,
    steps: &'static str,
    llm_mocks: &'static str,
    smoke_assertions: &'static str,
}

impl TemplateVars {
    fn new(name: &str, kind: PersonaTemplateKind) -> Self {
        let shape = TemplateShape::for_kind(kind);
        Self {
            persona_name: name.to_string(),
            persona_ident: name.to_string(),
            persona_title: to_title(name),
            package_slug: to_package_slug(name),
            template_kind: kind.as_str(),
            tools: shape.tools,
            tools_symbols: shape.tools_symbols,
            capabilities: shape.capabilities,
            autonomy_tier: shape.autonomy_tier,
            triggers: shape.triggers,
            budget: shape.budget,
            model_policy: shape.model_policy,
            persona_body: shape.persona_body,
            steps: shape.steps,
            llm_mocks: shape.llm_mocks,
            smoke_assertions: shape.smoke_assertions,
        }
    }
}

struct TemplateShape {
    tools: &'static str,
    tools_symbols: &'static str,
    capabilities: &'static str,
    autonomy_tier: &'static str,
    triggers: &'static str,
    budget: &'static str,
    model_policy: &'static str,
    persona_body: &'static str,
    steps: &'static str,
    llm_mocks: &'static str,
    smoke_assertions: &'static str,
}

impl TemplateShape {
    fn for_kind(kind: PersonaTemplateKind) -> Self {
        match kind {
            PersonaTemplateKind::DeterministicSweeper => Self {
                tools: r#"["github"]"#,
                tools_symbols: "[github]",
                capabilities: r#"["project.test_commands", "runtime.record_run"]"#,
                autonomy_tier: "act_with_approval",
                triggers: r#"["github.pr_opened"]"#,
                budget: "{ daily_usd = 2.0, run_usd = 0.05, max_tokens = 10000, max_runtime_seconds = 120 }",
                model_policy: r#"{ default_model = "mock", escalation_model = "mock", fallback_models = ["mock"], reasoning_effort = "low" }"#,
                persona_body: "  return drain_queue(task)",
                steps: DETERMINISTIC_STEPS,
                llm_mocks: "",
                smoke_assertions: "  assert_eq(result.status, \"completed\")\n  assert_eq(len(result.state.processed), 2)",
            },
            PersonaTemplateKind::HybridClassifyThenAct => Self {
                tools: r#"["github", "mcp"]"#,
                tools_symbols: "[github, mcp]",
                capabilities: r#"["project.scan", "runtime.record_run"]"#,
                autonomy_tier: "act_with_approval",
                triggers: r#"["github.issue_opened"]"#,
                budget: "{ daily_usd = 3.0, run_usd = 0.10, frontier_escalations = 1, max_tokens = 16000, max_runtime_seconds = 180 }",
                model_policy: r#"{ default_model = "mock", escalation_model = "mock", fallback_models = ["mock"], reasoning_effort = "medium" }"#,
                persona_body: "  let classified = classify_request(task)\n  return act_on_classification(classified)",
                steps: HYBRID_STEPS,
                llm_mocks: "",
                smoke_assertions: "  assert_eq(result.status, \"success\")\n  assert_eq(result.result.action, \"draft_plan\")",
            },
            PersonaTemplateKind::FrontierJudgmentLoop => Self {
                tools: r#"["github", "mcp"]"#,
                tools_symbols: "[github, mcp]",
                capabilities: r#"["project.scan", "runtime.record_run", "interaction.ask"]"#,
                autonomy_tier: "suggest",
                triggers: r#"["manual.review_requested"]"#,
                budget: "{ daily_usd = 5.0, run_usd = 0.25, frontier_escalations = 2, max_tokens = 24000, max_runtime_seconds = 300 }",
                model_policy: r#"{ default_model = "mock", escalation_model = "mock", fallback_models = ["mock"], reasoning_effort = "medium" }"#,
                persona_body: "  return judge_until_confident(task)",
                steps: FRONTIER_STEPS,
                llm_mocks: "  llm_mock({text: \"{\\\"decision\\\": \\\"ship\\\", \\\"confidence\\\": 0.92, \\\"rationale\\\": \\\"fixture is low risk\\\"}\"})",
                smoke_assertions: "  assert_eq(result.status, \"completed\")\n  assert_eq(result.state.decision, \"ship\")",
            },
        }
    }
}

const DETERMINISTIC_STEPS: &str = r#"@step(name: "drain_queue", receipt: audit, error_boundary: fail, budget: {max_tokens: 1000})
fn drain_queue(task) {
  let fixture = task?.fixture ?? {items: []}
  let prompt = render_prompt("../prompts/system.harn.prompt", {
    persona_name: "{{persona_name}}",
    template_kind: "{{template_kind}}",
    request: fixture?.request ?? "drain queued work",
  },)
  return bounded_loop(
    {remaining: fixture.items, processed: []},
    fn(state) {
      if len(state.remaining) == 0 {
        return {state: state, progress: false, done: true}
      }
      let item = state.remaining[0]
      let rest = state.remaining[1:]
      return {
        state: {remaining: rest, processed: state.processed + [item]},
        progress: true,
        done: len(rest) == 0,
      }
    },
    {
      persona: "{{persona_name}}",
      step: "drain_queue",
      started_at: task?.started_at,
      completed_at: task?.completed_at,
      max_iterations: 20,
      progress_required: true,
      metadata: {prompt_digest: sha256(prompt)},
    },
  )
}"#;

const HYBRID_STEPS: &str = r#"@step(name: "classify_request", model: "mock", receipt: audit, error_boundary: escalate, budget: {max_tokens: 2000})
fn classify_request(task) {
  let fixture = task?.fixture ?? {}
  let prompt = render_prompt("../prompts/system.harn.prompt", {
    persona_name: "{{persona_name}}",
    template_kind: "{{template_kind}}",
    request: fixture?.request ?? "classify requested work",
  },)
  return cheap_classify_then_escalate(
    fixture,
    fn(input) { return {label: "needs_plan", confidence: 0.62, item_count: len(input?.items ?? [])} },
    fn(input) { return {label: "draft_plan", confidence: 0.91, item_count: len(input?.items ?? [])} },
    fn(result) { return result.confidence < 0.7 || result.label == "needs_plan" },
    {
      persona: "{{persona_name}}",
      step: "classify_request",
      started_at: task?.started_at,
      completed_at: task?.completed_at,
      min_confidence: 0.7,
      metadata: {prompt_digest: sha256(prompt)},
    },
  )
}

@step(name: "act_on_classification", receipt: audit, error_boundary: fail, budget: {max_tokens: 1000})
fn act_on_classification(classified) {
  let wrapped = with_audit_receipt(
    fn() {
      return {
        action: classified.result.label,
        escalated: classified.escalated,
        confidence: classified.result.confidence,
      }
    },
    {persona: "{{persona_name}}", step: "act_on_classification"},
  )
  return wrapped()
}"#;

const FRONTIER_STEPS: &str = r#"@step(name: "judge_until_confident", model: "mock", receipt: audit, error_boundary: escalate, retry: {max_attempts: 2}, budget: {max_tokens: 4000})
fn judge_until_confident(task) {
  let fixture = task?.fixture ?? {}
  let schema = {
    type: "object",
    required: ["decision", "confidence", "rationale"],
    properties: {
      decision: {type: "string"},
      confidence: {type: "number"},
      rationale: {type: "string"},
    },
  }
  return bounded_loop(
    {decision: nil, confidence: 0.0, attempts: 0},
    fn(state) {
      let prompt = render_prompt("../prompts/system.harn.prompt", {
        persona_name: "{{persona_name}}",
        template_kind: "{{template_kind}}",
        request: fixture?.request ?? "review",
      },)
      let result = llm_call_structured_safe(prompt, schema, {
        provider: "mock",
        model: "mock",
        retries: 0,
      })
      if !result.ok {
        return {
          state: state + {attempts: state.attempts + 1, error: result.error},
          progress: true,
          done: state.attempts + 1 >= 2,
        }
      }
      return {
        state: result.data + {attempts: state.attempts + 1},
        progress: true,
        done: result.data.confidence >= 0.8,
      }
    },
    {
      persona: "{{persona_name}}",
      step: "judge_until_confident",
      started_at: task?.started_at,
      completed_at: task?.completed_at,
      max_iterations: 2,
      progress_required: true,
    },
  )
}"#;

fn render(template: &str, vars: &TemplateVars) -> String {
    template
        .replace("{{persona_body}}", vars.persona_body)
        .replace("{{steps}}", vars.steps)
        .replace("{{llm_mocks}}", vars.llm_mocks)
        .replace("{{smoke_assertions}}", vars.smoke_assertions)
        .replace("{{persona_name}}", &vars.persona_name)
        .replace("{{persona_ident}}", &vars.persona_ident)
        .replace("{{persona_title}}", &vars.persona_title)
        .replace("{{package_slug}}", &vars.package_slug)
        .replace("{{template_kind}}", vars.template_kind)
        .replace("{{tools}}", vars.tools)
        .replace("{{tools_symbols}}", vars.tools_symbols)
        .replace("{{capabilities}}", vars.capabilities)
        .replace("{{autonomy_tier}}", vars.autonomy_tier)
        .replace("{{triggers}}", vars.triggers)
        .replace("{{budget}}", vars.budget)
        .replace("{{model_policy}}", vars.model_policy)
}

#[allow(dead_code)]
pub(crate) fn template_dir_for_docs(kind: PersonaTemplateKind) -> PathBuf {
    Path::new("personas").join("_templates").join(kind.as_str())
}
