pub(super) fn agent_template_files(project_name: &str) -> Vec<(&'static str, String)> {
    vec![
        (
            "harn.toml",
            format!(
                r#"[package]
name = "{project_name}"
version = "0.1.0"

[skills]
paths = ["agent/skills"]

[dependencies]
"#
            ),
        ),
        (
            "main.harn",
            r#"import { run_agent_app } from "agent/app"

fn main(harness: Harness) {
  run_agent_app(harness)
}
"#
            .to_string(),
        ),
        (
            "agent/app.harn",
            r#"import { agent_host_tools } from "std/agent/host_tools"
import { audit_agent } from "std/agent/presets"

pub fn load_instructions(harness: Harness) -> string {
  let path = path_join(cwd(), "agent", "instructions.md")
  if harness.fs.exists(path) {
    return harness.fs.read_text(path)
  }
  return "Review the workspace carefully."
}

pub fn agent_app_tools(root) {
  return agent_host_tools(
    nil,
    {
      root: root,
      cwd: root,
      enabled_tools: ["list_directory", "read_file", "search_files"],
      max_inline_bytes: 6000,
      search_exclude_globs: [".git/**", ".harn/**", "target/**", "node_modules/**"],
    },
  )
}

pub fn run_agent_app_with_options(harness: Harness, options = nil) {
  let opts = options ?? {}
  let task = opts?.task ?? harness.env.get_or("HARN_TASK", "Review this workspace")
  let instructions = opts?.instructions ?? load_instructions(harness)
  let tools = opts?.tools ?? agent_app_tools(cwd())
  let result = audit_agent(
    task,
    {
      system: instructions,
      tools: tools,
      max_iterations: opts?.max_iterations ?? 6,
      loop_until_done: opts?.loop_until_done ?? true,
      llm_options: opts?.llm_options ?? {temperature: 0},
    },
  )
  if opts?.emit_result ?? true {
    harness.stdio.println(result.visible_text ?? result.text ?? "")
  }
  return result
}

pub fn run_agent_app(harness: Harness) {
  return run_agent_app_with_options(harness)
}
"#
            .to_string(),
        ),
        (
            "agent/instructions.md",
            r"# Agent Instructions

You are a careful workspace agent. Inspect before answering, keep changes scoped,
and prefer deterministic tools and task-plan stages over ad hoc prose protocols.

Use skills from `agent/skills` when they match the task. Keep subagent work
small, typed, and easy to replay.
"
            .to_string(),
        ),
        (
            "agent/tools/README.md",
            "# Tools\n\nPut app-specific tool helpers or manifests here.\n".to_string(),
        ),
        (
            "agent/subagents/README.md",
            "# Subagents\n\nPut focused worker/subagent definitions here.\n".to_string(),
        ),
        (
            "agent/channels/README.md",
            "# Channels\n\nPut channel trigger or connector notes here.\n".to_string(),
        ),
        (
            "agent/sandbox/README.md",
            "# Sandbox\n\nPut sandbox policy notes or fixtures here.\n".to_string(),
        ),
        (
            "agent/schedules/README.md",
            "# Schedules\n\nPut durable schedule definitions here.\n".to_string(),
        ),
        (
            "agent/skills/repository-review/SKILL.md",
            r"---
name: repository-review
short: Review repository context and risks.
description: Use when reviewing repository structure, risks, and next actions.
---

# Repository Review

Start by listing the relevant files, then inspect the narrowest source files
needed to answer. Report facts with file paths and keep recommendations small.
"
            .to_string(),
        ),
        (
            "tests/test_agent.harn",
            r#"import { run_agent_app_with_options } from "../agent/app"

pipeline test_agent_smoke(task) {
  llm_mock_clear()
  llm_mock({text: "<user_response>Repository looks healthy.</user_response>"})
  let result = run_agent_app_with_options(
    harness,
    {
      task: "Review the repository",
      instructions: "Return a concise repository health note.",
      tools: tool_registry(),
      max_iterations: 1,
      loop_until_done: false,
      llm_options: {provider: "mock", model: "mock", temperature: 0},
      emit_result: false,
    },
  )
  assert_eq(result.status, "done")
}
"#
            .to_string(),
        ),
        (
            "README.md",
            format!(
                r#"# {project_name}

Opinionated Harn agent app starter.

## Layout

- `main.harn` is the entrypoint.
- `agent/instructions.md` is the default system instruction source.
- `agent/app.harn` wires the instructions and scoped tools into existing Harn agent primitives.
- `agent/skills/` is enabled in `harn.toml` for progressive skill loading.
- `agent/tools/`, `agent/subagents/`, `agent/channels/`, `agent/sandbox/`, and `agent/schedules/` are convention folders for app-specific capabilities and manifests.

## Run

```bash
HARN_TASK="Summarize this repository" harn run main.harn
```

Run `harn quickstart` first if this project does not already have an LLM
provider configured. The starter uses scoped read-only tools by default; add
write or command tools only when the task needs them.
"#
            ),
        ),
    ]
}
