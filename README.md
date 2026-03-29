# Harn

[![CI](https://github.com/burin-labs/harn/actions/workflows/ci.yml/badge.svg)](https://github.com/burin-labs/harn/actions/workflows/ci.yml)

A programming language for orchestrating AI agents.

Harn gives you pipelines, concurrency, LLM calls, error recovery,
destructuring, a built-in test framework, and sandboxed execution as
language primitives — not library abstractions. Instead of wiring together
agents in Python with callbacks and retry decorators, you write this:

```javascript
pipeline default(task) {
  let plan = llm_call(task, "Break this into steps")

  let results = parallel_map(json_parse(plan)) { step ->
    retry 3 {
      agent_loop(step, "You are a coding assistant", {persistent: true})
    }
  }

  write_file("output.json", json_stringify(results))
}
```

## Getting started

### Install

From a [GitHub release](https://github.com/burin-labs/harn/releases)
(macOS and Linux):

```bash
curl -fsSL https://raw.githubusercontent.com/burin-labs/harn/main/install.sh | sh
```

With Cargo (recommended for Rust users):

```bash
cargo install harn-cli
```

Or build from source:

```bash
git clone https://github.com/burin-labs/harn.git
cd harn && cargo install --path crates/harn-cli
```

### Create a project

```bash
harn init my-project
cd my-project
```

This scaffolds `main.harn`, `lib/helpers.harn`, and `tests/test_main.harn`.

### Run it

```bash
harn run main.harn
```

### Run the tests

```bash
harn test tests/
```

Any pipeline named `test_*` is discovered and executed automatically.
Use `assert`, `assert_eq`, and `assert_ne` for assertions:

```javascript
pipeline test_math(task) {
  assert_eq(2 + 2, 4)
  assert(10 > 5)
  assert_ne("hello", "world")
}
```

### Try the REPL

```bash
harn repl
```

### Make an LLM call

Set your API key and call a model directly from the language:

```bash
export ANTHROPIC_API_KEY="sk-..."
harn run examples/llm-call.harn
```

```javascript
pipeline default(task) {
  let response = llm_call(
    "What is 2 + 2? Answer with just the number.",
    "You are a helpful assistant. Be concise."
  )
  log("LLM says: ${response}")
}
```

Harn supports Anthropic, OpenAI, Ollama, and OpenRouter out of the box.

### Build an agent with tools

Register tools with JSON Schema-compatible definitions, then let the LLM
call them:

```javascript
pipeline default(task) {
  let tools = tool_registry()
  let tools = tool_add(tools, "search", "Search the web", { query ->
    return http_get("https://api.search.com?q=" + query).body
  }, {query: "string"})

  let tools = tool_add(tools, "calculate", "Evaluate math", { expr ->
    return shell("echo '" + expr + "' | bc").stdout
  }, {expression: "string"})

  // Generate a universal prompt that works with any LLM
  let system = tool_prompt(tools)

  let response = llm_call(task, system)

  // Parse and dispatch tool calls from LLM output
  let call = tool_parse_call(response)
  if call != nil {
    let tool = tool_find(tools, call.name)
    let handler = tool.handler
    let result = handler(call.arguments.query)
    log(result)
  }
}
```

## What the language looks like

### Data transformation with pipes

```javascript
pipeline default(task) {
  let users = [
    {name: "Alice", age: 30, role: "engineer"},
    {name: "Bob", age: 25, role: "designer"},
    {name: "Charlie", age: 35, role: "engineer"}
  ]

  let senior_engineers = users
    |> { list -> list.filter({ u -> u.role == "engineer" }) }
    |> { list -> list.filter({ u -> u.age >= 30 }) }
    |> { list -> list.map({ u -> u.name }) }

  log(senior_engineers)  // ["Alice", "Charlie"]
}
```

### Persistent agent loops

The `agent_loop` builtin maintains conversation history across turns and
keeps the agent working until it emits a `##DONE##` sentinel:

```javascript
pipeline default(task) {
  let result = agent_loop(
    task,
    "You are a coding assistant.",
    {persistent: true, max_nudges: 3, max_iterations: 50}
  )
  log(result.text)
}
```

### Agent loops with tools

`agent_loop` can execute tools during the conversation loop. Pass tool
names as a string list to use built-in schemas, or provide a
`tool_registry` or raw tool dicts. By default, tools are invoked via
text-based `<tool_call>` XML tags, which works with any model. Set
`tool_format: "native"` to use API-level function calling instead.

```javascript
pipeline default(task) {
  let result = agent_loop(
    task,
    "You are a coding assistant. Use tools to complete the task.",
    {
      persistent: true,
      tools: ["read_file", "search", "edit", "run"],
      tool_format: "text",      // default; or "native" for function calling
      max_iterations: 25
    }
  )
  log(result.text)
}
```

Built-in tool schemas: `read_file`, `search`, `edit`, `run`, `outline`,
`web_search`, `web_fetch`, `lsp_hover`, `lsp_definition`,
`lsp_references`, `list_directory`.

In ACP/bridge mode, `register_agent_loop_with_bridge()` wires tool
execution through the host so the editor or CLI handles the actual I/O.

### Parallel execution

Run work concurrently without callbacks or async/await noise:

```javascript
pipeline default(task) {
  let files = ["src/main.rs", "src/lib.rs", "src/utils.rs"]

  let analyses = parallel_map(files) { file ->
    llm_call(read_file(file), "Review this code for bugs")
  }

  for a in analyses {
    log(a)
  }
}
```

### HTTP with retries and timeouts

Production-ready HTTP with automatic retries and exponential backoff:

```javascript
pipeline default(task) {
  // Full request with options
  let r = http_request("POST", "https://api.example.com/data", {
    body: json_stringify({query: task}),
    headers: {content_type: "application/json"},
    auth: "Bearer sk-...",
    timeout: 5000,
    retries: 3,
    backoff: 1000
  })

  if r.ok {
    log(json_parse(r.body))
  }

  // Shorthand helpers
  let page = http_get("https://example.com")
  let resp = http_post("https://api.example.com", json_stringify({x: 1}))
}
```

### Structured logging

JSON-structured logs with levels, fields, and trace context:

```javascript
pipeline default(task) {
  let span = trace_start("process_request")

  log_info("processing", {task: task, step: "start"})

  // All log calls within a trace include trace_id automatically
  log_debug("details", {verbose: true})
  log_warn("slow response", {latency_ms: 500})

  trace_end(span)

  // Filter by level
  log_set_level("warn")  // only warn and error after this
}
```

### Composable pipelines

Pipelines can extend and override each other:

```javascript
pipeline base(task) {
  let context = read_file("README.md")
  log("Context loaded")
}

pipeline deploy(task) extends base {
  override fn setup() { /* custom setup */ }
  log("Deploying...")
}
```

### Shared libraries

Factor common logic into library files and import them:

```javascript
// lib/helpers.harn
fn double(x) { return x * 2 }
fn greet(name) { return "hello " + name }
```

```javascript
import "lib/helpers"

pipeline default(task) {
  log(greet("world"))
  log(double(21))
}
```

## Documentation

- [Why Harn?](docs/why-harn.md) — what problem Harn solves, how it compares, who it's for
- [Cookbook](docs/cookbook.md) — practical patterns for agentic programming with working examples
- [Language basics](docs/language-basics.md) — syntax, types, operators, control flow, functions, collections
- [LLM calls and agent loops](docs/llm-and-agents.md) — providers, API keys, `llm_call`, `agent_loop`, persistent mode
- [Concurrency](docs/concurrency.md) — `spawn`/`await`, `parallel`, `parallel_map`, channels, atomics, mutex, deadline
- [Error handling](docs/error-handling.md) — `try`/`catch`/`throw`, `retry`, typed catch
- [Modules and imports](docs/modules.md) — library files, `import`, pipeline inheritance
- [Builtin functions](docs/builtins.md) — complete reference for all built-in functions
- [Language specification](spec/HARN_SPEC.md) — formal spec covering lexical rules, grammar, and semantics
- [AST reference](spec/AST.md) — node types used by the parser

## Tooling

Harn ships with built-in formatting, linting, and testing:

```bash
# Scaffold a new project
harn init my-project

# Format code (opinionated, 2-space indent)
harn fmt myfile.harn
harn fmt --check myfile.harn  # check without modifying

# Lint code
harn lint myfile.harn

# Run tests (discovers test_* pipelines)
harn test tests/
harn test myfile_test.harn
```

The linter catches: unused variables, unreachable code, `var` that should
be `let`, empty blocks, and shadowed variables.

### Sandbox mode

Run untrusted code with restricted filesystem and network access:

```bash
harn run --sandbox script.harn
```

In sandbox mode, file operations are confined to the working directory
and network calls are disabled by default.

Errors render with source context, like Rust:

```text
error: undefined variable `reponse`
  --> pipeline.harn:12:15
   |
12 |   let output = reponse
   |                ^^^^^^^ not found in this scope
   |
   = help: did you mean `response`?
```

### Editor support

**VS Code** — syntax highlighting, hover, completions, go-to-definition,
diagnostics, and linting:

```bash
cd editors/vscode
npm install && npm run compile
npx vsce package
code --install-extension harn-lang-0.1.0.vsix
```

Or open `editors/vscode/` in VS Code and press F5 to launch a dev host.

## For language designers

Harn's execution pipeline, written in Rust:

```text
source -> Lexer -> Parser -> TypeChecker -> Compiler -> VM (bytecode)
```

The codebase is organized as a Cargo workspace:

| Crate | Purpose |
|---|---|
| `harn-lexer` | Tokenizer with byte-offset span tracking |
| `harn-parser` | Parser, spanned AST (`SNode`), type checker, diagnostic renderer |
| `harn-vm` | Bytecode compiler, VM, and all builtin functions |
| `harn-fmt` | Opinionated code formatter |
| `harn-lint` | Linter with 5 rules |
| `harn-cli` | CLI: `run`, `test`, `repl`, `init`, `fmt`, `lint`, `version`, `acp`, `serve` |
| `harn-lsp` | Language Server Protocol |
| `harn-dap` | Debug Adapter Protocol |
| `harn-wasm` | WASM target (built separately with wasm-pack) |

The [language specification](spec/HARN_SPEC.md) is the authoritative reference.
The tree-sitter grammar for editor support is in `tree-sitter-harn/`.

## For contributors

```bash
# Run everything: format, lint, test, conformance
make all

# Individual checks
make fmt          # auto-format
make lint         # clippy (warnings are errors)
make test         # Rust unit tests
make conformance  # language conformance tests
```

Conformance tests in `conformance/` are the primary way to verify language
behavior. Each test is a `.harn` file paired with a `.expected` or `.error`
file. Add one whenever you change the parser or VM.

Pre-commit hooks run `fmt` and `clippy` automatically. After cloning, set them up:

```bash
git config core.hooksPath .githooks
```
