use std::fs;
use std::path::{Path, PathBuf};
use std::process;

use crate::cli::{NewArgs, ProjectTemplate};
use crate::dispatch;
use crate::env_guard::ScopedEnvVar;
use crate::package::current_harn_range_example;

pub(crate) fn resolve_new_args(
    args: &NewArgs,
) -> Result<(Option<String>, ProjectTemplate), String> {
    let template = args.template.unwrap_or(ProjectTemplate::Basic);
    match (args.first.as_deref(), args.second.as_deref()) {
        (Some("package"), Some(name)) => Ok((Some(name.to_string()), ProjectTemplate::Package)),
        (Some("connector"), Some(name)) => Ok((Some(name.to_string()), ProjectTemplate::Connector)),
        (Some(kind @ ("package" | "connector")), None) => Err(format!(
            "`harn new {kind}` requires a package name, for example `harn new {kind} my-{kind}`"
        )),
        (Some(name), None) => Ok((Some(name.to_string()), template)),
        (None, None) => Ok((None, template)),
        (Some(_), Some(_)) => Err(
            "unexpected second positional argument; use `harn new package NAME` or `harn new NAME --template package`"
                .to_string(),
        ),
        (None, Some(_)) => unreachable!("clap cannot fill second positional without first"),
    }
}

/// `harn init` and `harn new` dispatch shim. Resolves the destination
/// directory in Rust (matches the legacy behavior that errors if a
/// non-`init` name already exists), then delegates the template render +
/// file-write loop to `cli/scaffold/init.harn` (harn#2308 / W8 of the
/// harn-cli self-host epic). The legacy Rust implementation stays
/// behind `HARN_CLI_IMPL=rust` until the C1 ratchet (#2314) removes it.
pub(crate) async fn init_project(name: Option<&str>, template: ProjectTemplate) {
    let dir = match name {
        Some(n) => {
            let dir = PathBuf::from(n);
            if dir.exists() {
                eprintln!("Directory '{n}' already exists");
                process::exit(1);
            }
            fs::create_dir_all(&dir).unwrap_or_else(|e| {
                eprintln!("Failed to create directory: {e}");
                process::exit(1);
            });
            dir
        }
        None => PathBuf::from("."),
    };

    let project_name = name
        .and_then(|value| Path::new(value).file_name().and_then(|name| name.to_str()))
        .unwrap_or("my-project")
        .to_string();

    if std::env::var("HARN_CLI_IMPL").as_deref() == Ok("rust") {
        init_project_rust(name, &dir, &project_name, template);
        return;
    }

    let exit = dispatch_to_script(name, &dir, &project_name, template).await;
    if exit != 0 {
        process::exit(exit);
    }
}

async fn dispatch_to_script(
    name: Option<&str>,
    dir: &Path,
    project_name: &str,
    template: ProjectTemplate,
) -> i32 {
    let dir_str = dir.display().to_string();
    let template_id = template_id(template);
    let harn_range = current_harn_range_example();
    let name_str = name.unwrap_or("");
    let _name_env = ScopedEnvVar::set("HARN_INIT_NAME", name_str);
    let _project_env = ScopedEnvVar::set("HARN_INIT_PROJECT_NAME", project_name);
    let _dir_env = ScopedEnvVar::set("HARN_INIT_DIR", &dir_str);
    let _template_env = ScopedEnvVar::set("HARN_INIT_TEMPLATE", template_id);
    let _range_env = ScopedEnvVar::set("HARN_INIT_HARN_RANGE", &harn_range);
    let _mode_env = ScopedEnvVar::set(
        "HARN_INIT_MODE",
        if name.is_some() { "new" } else { "init" },
    );
    dispatch::dispatch_to_embedded_script("scaffold/init", Vec::new(), /* json_mode */ false).await
}

fn template_id(template: ProjectTemplate) -> &'static str {
    match template {
        ProjectTemplate::Basic => "basic",
        ProjectTemplate::Agent => "agent",
        ProjectTemplate::Chat => "chat",
        ProjectTemplate::McpServer => "mcp-server",
        ProjectTemplate::Eval => "eval",
        ProjectTemplate::PipelineLab => "pipeline-lab",
        ProjectTemplate::Package => "package",
        ProjectTemplate::Connector => "connector",
    }
}

/// Legacy Rust implementation. Kept behind `HARN_CLI_IMPL=rust` for the
/// parity harness (#2299). The C1 ratchet (#2314) removes this path
/// once the `.harn` impl is the production default everywhere.
fn init_project_rust(
    name: Option<&str>,
    dir: &Path,
    project_name: &str,
    template: ProjectTemplate,
) {
    if let Some(n) = name {
        println!("Creating project '{n}'...");
    } else {
        println!("Initializing harn project in current directory...");
    }
    for (relative_path, content) in template_files(project_name, template) {
        write_if_new(&dir.join(relative_path), &content);
    }

    println!();
    if let Some(n) = name {
        println!("  cd {n}");
    }
    match template {
        ProjectTemplate::Basic
        | ProjectTemplate::Agent
        | ProjectTemplate::Eval
        | ProjectTemplate::Package
        | ProjectTemplate::Connector => {
            println!("  harn run main.harn       # run the program");
            println!("  harn test tests/         # run the tests");
        }
        ProjectTemplate::Chat => {
            println!("  harn run main.harn       # run the program");
        }
        ProjectTemplate::McpServer => {
            println!("  harn serve mcp main.harn # expose the starter MCP server");
        }
        ProjectTemplate::PipelineLab => {
            println!("  harn playground --task \"Explain this repo\"    # run the lab");
            println!("  harn playground --watch --task \"Refine the prompt\"  # live iteration");
        }
    }
    println!("  harn fmt main.harn       # format code");
    println!("  harn lint main.harn      # lint code");
    if matches!(
        template,
        ProjectTemplate::Package | ProjectTemplate::Connector
    ) {
        println!("  harn package check       # validate publish readiness");
        println!("  harn package docs        # generate API docs");
        println!("  harn package pack        # build an inspectable artifact");
    }
    println!("  harn doctor              # verify the local environment");
}

fn write_if_new(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            eprintln!("Failed to create {}: {e}", parent.display());
            return;
        }
    }
    if path.exists() {
        println!("  skip  {} (already exists)", path.display());
    } else {
        fs::write(path, content).unwrap_or_else(|e| {
            eprintln!("Failed to write {}: {e}", path.display());
        });
        println!("  create  {}", path.display());
    }
}

fn template_files(project_name: &str, template: ProjectTemplate) -> Vec<(&'static str, String)> {
    let harn_range = current_harn_range_example();
    let manifest = format!(
        r#"[package]
name = "{project_name}"
version = "0.1.0"

[dependencies]
"#
    );

    match template {
        ProjectTemplate::Basic => vec![
            ("harn.toml", manifest),
            (
                "main.harn",
                r#"import "lib/helpers"

pipeline default(task) {
  let greeting = greet("world")
  log(greeting)
}
"#
                .to_string(),
            ),
            (
                "lib/helpers.harn",
                r#"fn greet(name) {
  return "Hello, " + name + "!"
}

fn add(a, b) {
  return a + b
}
"#
                .to_string(),
            ),
            (
                "tests/test_main.harn",
                r#"import "../lib/helpers"

pipeline test_greet(task) {
  assert_eq(greet("world"), "Hello, world!")
  assert_eq(greet("Harn"), "Hello, Harn!")
}

pipeline test_add(task) {
  assert_eq(add(2, 3), 5)
  assert_eq(add(-1, 1), 0)
  assert_eq(add(0, 0), 0)
}
"#
                .to_string(),
            ),
        ],
        ProjectTemplate::Agent => vec![
            ("harn.toml", manifest),
            (
                "main.harn",
                r#"import { agent_host_tools } from "std/agent/host_tools"
import { audit_agent } from "std/agent/presets"

fn main(harness: Harness) {
  let task = harness.env.get_or("HARN_TASK", "Review the repository")
  let root = cwd()
  let tools = agent_host_tools(
    nil,
    {
      root: root,
      cwd: root,
      enabled_tools: ["list_directory", "read_file", "search_files"],
      max_inline_bytes: 6000,
      search_exclude_globs: [".git/**", ".harn/**", "target/**", "node_modules/**"],
    },
  )
  let result = audit_agent(
    task,
    {
      system: "You are a careful repository auditor. Use the read-only tools to inspect before answering. Do not guess file contents, and do not discuss provider, model, harness, or system-prompt details.",
      tools: tools,
      max_iterations: 6,
      llm_options: {temperature: 0},
    },
  )
  harness.stdio.println(result.visible_text ?? result.text ?? "")
}
"#
                .to_string(),
            ),
            (
                "tests/test_agent.harn",
                r#"import { audit_agent } from "std/agent/presets"

pipeline test_agent_smoke(task) {
  llm_mock_clear()
  llm_mock({text: "<user_response>Repository looks healthy.</user_response>\n<done>##DONE##</done>"})
  let result = audit_agent(
    "Review the repository",
    {provider: "mock", model: "mock", loop_until_done: false, max_iterations: 1},
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

Read-only agent starter for Harn.

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
        ],
        ProjectTemplate::Chat => vec![
            (
                "harn.toml",
                format!(
                    r#"[package]
name = "{project_name}"
version = "0.1.0"

[dependencies]

[llm.aliases]
chat = {{ id = "llama3.2", provider = "ollama" }}
"#
                ),
            ),
            (
                "main.harn",
                r#"fn build_chat_prompt(history, message) {
  if history == "" {
    return "User: " + message
  }
  return history + "\nUser: " + message
}

fn stream_reply(harness: Harness, prompt, system, options) {
  var answer = ""
  let chunks = llm_stream_call(prompt, system, options)
  for chunk in chunks {
    harness.stdio.print(chunk.visible_delta)
    answer = chunk.partial
  }
  harness.stdio.println("")
  return answer
}

fn main(harness: Harness) {
  let system = env_or("HARN_CHAT_SYSTEM", "You are a concise, helpful assistant.")
  let model = env_or("HARN_CHAT_MODEL", "chat")
  let options = {model: model, max_tokens: 2048, temperature: 0.7}
  var history = ""
  harness.stdio.println("Harn chat. Type /clear to reset history or /exit to quit.")
  while true {
    harness.stdio.print("you> ")
    let raw = harness.stdio.read_line()
    if !raw.ok {
      break
    }
    let message = trim(raw.value)
    if message == "" {
      continue
    }
    if message == "/exit" || message == "/quit" {
      break
    }
    if message == "/clear" {
      history = ""
      harness.stdio.println("history cleared")
      continue
    }
    let prompt = build_chat_prompt(history, message)
    harness.stdio.print("assistant> ")
    let answer = stream_reply(harness, prompt, system, options)
    history = prompt + "\nAssistant: " + answer + "\n"
  }
}
"#
                .to_string(),
            ),
            (
                "README.md",
                format!(
                    r#"# {project_name}

Streaming chat starter for Harn.

## Provider

`harn.toml` defines a `chat` model alias backed by Ollama's `llama3.2` model:

```toml
[llm.aliases]
chat = {{ id = "llama3.2", provider = "ollama" }}
```

Edit that alias to use any configured provider, or set `HARN_CHAT_MODEL` when
running the project.

## Run

```bash
harn run main.harn
```

Inside the chat loop, type `/clear` to reset local history and `/exit` to quit.
"#
                ),
            ),
        ],
        ProjectTemplate::McpServer => vec![
            ("harn.toml", manifest),
            (
                "main.harn",
                r#"pipeline default(task) {
  var tools = tool_registry()
  tools = tool_define(tools, "ping", "Return a pong response", {
    parameters: {
      type: "object",
      properties: {
        message: {type: "string"}
      },
      required: ["message"]
    },
    returns: {
      type: "object",
      properties: {
        message: {type: "string"},
        echoed: {type: "string"}
      }
    },
    handler: fn(args) {
      return {
        message: "pong",
        echoed: args.message
      }
    }
  })

  mcp_tools(tools)
  mcp_resource({
    uri: "info://server",
    name: "server-info",
    text: "Harn MCP starter server"
  })
}
"#
                .to_string(),
            ),
        ],
        ProjectTemplate::Eval => vec![
            ("harn.toml", manifest),
            (
                "main.harn",
                r#"pipeline default(task) {
  let input = "hello world"
  let output = input.upper()
  let passed = output == "HELLO WORLD"

  eval_metric("passed", passed)
  eval_metric("output_length", len(output))

  log(json_stringify({
    input: input,
    output: output,
    passed: passed
  }))
}
"#
                .to_string(),
            ),
            (
                "tests/test_eval.harn",
                r#"pipeline test_eval_metrics(task) {
  eval_metric("accuracy", 1.0, {suite: "smoke"})
  let metrics = eval_metrics()

  assert_eq(len(metrics), 1)
  assert_eq(metrics[0].name, "accuracy")
}
"#
                .to_string(),
            ),
            (
                "eval-suite.json",
                r#"{
  "_type": "eval_suite_manifest",
  "id": "sample-suite",
  "name": "Sample Eval Suite",
  "base_dir": ".",
  "cases": [
    {
      "label": "replace-with-a-run-record",
      "run_path": ".harn-runs/sample-run.json"
    }
  ]
}
"#
                .to_string(),
            ),
        ],
        ProjectTemplate::PipelineLab => vec![
            ("harn.toml", manifest),
            (
                "host.harn",
                r"pub fn build_context(task) {
  return {
    task: task,
    cwd: cwd(),
  }
}

pub fn request_permission(tool_name, request_args) -> bool {
  return true
}
"
                .to_string(),
            ),
            (
                "pipeline.harn",
                r#"pipeline default(task) {
  let context = build_context(env_or("HARN_TASK", ""))
  let result = llm_call(
    "Task: " + context.task + "\nWorkspace: " + context.cwd,
    "You are a concise coding assistant. Reply in 3 bullets max.",
  )
  log(result.text)
}
"#
                .to_string(),
            ),
            (
                "README.md",
                r#"# Pipeline Lab

Use this project to iterate on a Harn workflow against a local Harn-native host module.

## Run

```bash
harn playground --task "Explain this repository"
```

## Watch mode

```bash
harn playground --watch --task "Tighten the workflow prompt"
```

Edit `host.harn` or `pipeline.harn` and the playground will re-run automatically.
"#
                .to_string(),
            ),
        ],
        ProjectTemplate::Package => vec![
            (
                "harn.toml",
                format!(
                    r#"[package]
name = "{project_name}"
version = "0.1.0"
description = "Reusable Harn package."
license = "MIT OR Apache-2.0"
repository = "https://github.com/OWNER/{project_name}"
harn = "{harn_range}"
docs_url = "docs/api.md"

[exports]
lib = "lib/main.harn"

[dependencies]
"#
                ),
            ),
            (
                "lib/main.harn",
                r#"/// Return a greeting for `name`.
pub fn greet(name: string) -> string {
  return "Hello, " + name + "!"
}
"#
                .to_string(),
            ),
            (
                "main.harn",
                r#"import "lib/main"

pipeline default(task) {
  log(greet("world"))
}
"#
                .to_string(),
            ),
            (
                "tests/test_main.harn",
                r#"import "../lib/main"

pipeline test_greet(task) {
  assert_eq(greet("Harn"), "Hello, Harn!")
}
"#
                .to_string(),
            ),
            (
                ".github/workflows/harn-package.yml",
                r"name: Harn package

on:
  pull_request:
  push:
    branches: [main]

jobs:
  package:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: taiki-e/install-action@cargo-binstall
      - run: cargo binstall harn-cli --no-confirm
      - run: harn install --locked --offline || harn install
      - run: harn test tests/
      - run: harn package check
      - run: harn package docs --check
      - run: harn package pack --dry-run
"
                .to_string(),
            ),
            (
                "README.md",
                format!(
                    r"# {project_name}

Reusable Harn package.

## Quickstart

```bash
harn add ../{project_name}
harn test tests/
harn package check
harn package docs
harn package pack
```

Consumers import stable modules through the `[exports]` entries in `harn.toml`.
"
                ),
            ),
            (
                "LICENSE",
                "MIT OR Apache-2.0\n".to_string(),
            ),
            (
                "docs/api.md",
                format!(
                    r"# API Reference: {project_name}

Generated by `harn package docs`.

Version: `0.1.0`

## Export `lib`

`lib/main.harn`

### fn `greet`

Return a greeting for `name`.

```harn
pub fn greet(name: string) -> string
```
"
                ),
            ),
        ],
        ProjectTemplate::Connector => vec![
            (
                "harn.toml",
                format!(
                    r#"[package]
name = "{project_name}"
version = "0.1.0"
description = "Pure-Harn connector package."
license = "MIT OR Apache-2.0"
repository = "https://github.com/OWNER/{project_name}"
harn = "{harn_range}"
docs_url = "docs/api.md"

[exports]
connector = "connectors/echo.harn"

[[providers]]
id = "echo"
connector = {{ harn = "connectors/echo" }}

[connector_contract]
version = 1

[[connector_contract.fixtures]]
provider = "echo"
name = "message"
kind = "webhook"
body_json = {{ message = "hello" }}
expect_type = "event"
expect_kind = "webhook"
expect_event_count = 1

[dependencies]
"#
                ),
            ),
            (
                "connectors/echo.harn",
                r#"/// Connector provider id.
pub fn provider_id() {
  return "echo"
}

/// Trigger kinds emitted by this connector.
pub fn kinds() {
  return ["webhook"]
}

/// JSON payload schema for normalized inbound events.
pub fn payload_schema() {
  return {
    harn_schema_name: "EchoEventPayload",
    json_schema: {
      type: "object",
      additionalProperties: true,
    },
  }
}

/// Convert one inbound request into Harn trigger events.
pub fn normalize_inbound(raw) {
  let body = raw.body_json ?? json_parse(raw.body_text)
  return {
    type: "event",
    event: {
      kind: "webhook",
      dedupe_key: "echo:" + (body.message ?? "message"),
      payload: body,
    },
  }
}
"#
                .to_string(),
            ),
            (
                "main.harn",
                r#"import "connectors/echo"

pipeline default(task) {
  log(provider_id())
}
"#
                .to_string(),
            ),
            (
                "tests/test_connector.harn",
                r#"import "../connectors/echo"

pipeline test_provider_id(task) {
  assert_eq(provider_id(), "echo")
  assert_eq(kinds(), ["webhook"])
}
"#
                .to_string(),
            ),
            (
                ".github/workflows/harn-package.yml",
                r"name: Harn connector package

on:
  pull_request:
  push:
    branches: [main]

jobs:
  package:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: taiki-e/install-action@cargo-binstall
      - run: cargo binstall harn-cli --no-confirm
      - run: harn install --locked --offline || harn install
      - run: harn connector test .
      - run: harn package check
      - run: harn package docs --check
      - run: harn package pack --dry-run
"
                .to_string(),
            ),
            (
                "README.md",
                format!(
                    r"# {project_name}

Pure-Harn connector package.

## Quickstart

```bash
harn connector test .
harn package check
harn package docs
harn package pack
```

Consumers import the connector through the stable `[exports]` entry in `harn.toml`.
"
                ),
            ),
            ("LICENSE", "MIT OR Apache-2.0\n".to_string()),
            (
                "docs/api.md",
                format!(
                    r"# API Reference: {project_name}

Generated by `harn package docs`.

Version: `0.1.0`

## Export `connector`

`connectors/echo.harn`

### fn `provider_id`

Connector provider id.

```harn
pub fn provider_id()
```

### fn `kinds`

Trigger kinds emitted by this connector.

```harn
pub fn kinds()
```

### fn `payload_schema`

JSON payload schema for normalized inbound events.

```harn
pub fn payload_schema()
```

### fn `normalize_inbound`

Convert one inbound request into Harn trigger events.

```harn
pub fn normalize_inbound(raw)
```
"
                ),
            ),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::{resolve_new_args, template_files};
    use crate::cli::{NewArgs, ProjectTemplate};
    use crate::package::current_harn_range_example;

    #[test]
    fn basic_template_keeps_library_layout() {
        let files = template_files("sample", ProjectTemplate::Basic);
        let paths: Vec<&str> = files.iter().map(|(path, _)| *path).collect();
        assert!(paths.contains(&"lib/helpers.harn"));
        assert!(paths.contains(&"tests/test_main.harn"));
    }

    #[test]
    fn new_templates_include_expected_entrypoints() {
        let agent = template_files("sample", ProjectTemplate::Agent);
        assert!(agent.iter().any(|(path, _)| *path == "main.harn"));
        assert!(agent
            .iter()
            .any(|(path, _)| *path == "tests/test_agent.harn"));

        let mcp = template_files("sample", ProjectTemplate::McpServer);
        assert!(mcp.iter().any(|(path, _)| *path == "main.harn"));

        let chat = template_files("sample", ProjectTemplate::Chat);
        assert!(chat.iter().any(|(path, _)| *path == "main.harn"));
        assert!(chat.iter().any(|(path, _)| *path == "README.md"));

        let eval = template_files("sample", ProjectTemplate::Eval);
        assert!(eval.iter().any(|(path, _)| *path == "eval-suite.json"));

        let pipeline_lab = template_files("sample", ProjectTemplate::PipelineLab);
        assert!(pipeline_lab.iter().any(|(path, _)| *path == "host.harn"));
        assert!(pipeline_lab
            .iter()
            .any(|(path, _)| *path == "pipeline.harn"));

        let package = template_files("sample", ProjectTemplate::Package);
        assert!(package.iter().any(|(path, _)| *path == "lib/main.harn"));
        assert!(package
            .iter()
            .any(|(path, _)| *path == ".github/workflows/harn-package.yml"));
        assert!(package.iter().any(|(path, content)| {
            *path == "harn.toml"
                && content.contains(&format!("harn = \"{}\"", current_harn_range_example()))
        }));

        let connector = template_files("sample", ProjectTemplate::Connector);
        assert!(connector
            .iter()
            .any(|(path, _)| *path == "connectors/echo.harn"));
        assert!(connector.iter().any(|(path, content)| {
            *path == "harn.toml"
                && content.contains(&format!("harn = \"{}\"", current_harn_range_example()))
        }));
    }

    #[test]
    fn chat_template_wires_streaming_loop_to_configured_alias() {
        let files = template_files("sample", ProjectTemplate::Chat);
        let content = |path: &str| {
            files
                .iter()
                .find_map(|(candidate, content)| (*candidate == path).then_some(content.as_str()))
                .unwrap_or_else(|| panic!("missing {path}"))
        };

        assert!(content("main.harn").contains("llm_stream_call(prompt, system, options)"));
        assert!(content("main.harn").contains("harness.stdio.read_line()"));
        assert!(content("harn.toml").contains(r#"chat = { id = "llama3.2", provider = "ollama" }"#));
        toml::from_str::<toml::Value>(content("harn.toml")).expect("chat manifest is valid TOML");
    }

    #[test]
    fn new_package_kind_resolves_to_package_template() {
        let args = NewArgs {
            first: Some("package".to_string()),
            second: Some("sample".to_string()),
            template: None,
        };
        let (name, template) = resolve_new_args(&args).unwrap();
        assert_eq!(name.as_deref(), Some("sample"));
        assert_eq!(template, ProjectTemplate::Package);
    }
}
