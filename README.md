# Harn

[![CI](https://github.com/burin-labs/harn/actions/workflows/ci.yml/badge.svg)](https://github.com/burin-labs/harn/actions/workflows/ci.yml)

A programming language for orchestrating AI agents.

Harn gives you pipelines, concurrency, LLM calls, and error recovery as
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

### Install and build

```bash
git clone https://github.com/burin-labs/harn.git
cd harn
cargo build
```

### Run your first program

Create `hello.harn`:

```javascript
pipeline default(task) {
  log("Hello, Harn!")
}
```

```bash
cargo run --bin harn -- run hello.harn
```

### Try the REPL

```bash
cargo run --bin harn -- repl
```

### Make an LLM call

Set your API key and call a model directly from the language:

```bash
export ANTHROPIC_API_KEY="sk-..."
cargo run --bin harn -- run examples/llm-call.harn
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
  log(result)
}
```

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

- [Language basics](docs/language-basics.md) — syntax, types, operators, control flow, functions, collections
- [LLM calls and agent loops](docs/llm-and-agents.md) — providers, API keys, `llm_call`, `agent_loop`, persistent mode
- [Concurrency](docs/concurrency.md) — `spawn`/`await`, `parallel`, `parallel_map`, channels, atomics, mutex, deadline
- [Error handling](docs/error-handling.md) — `try`/`catch`/`throw`, `retry`, typed catch
- [Modules and imports](docs/modules.md) — library files, `import`, pipeline inheritance
- [Builtin functions](docs/builtins.md) — complete reference for all built-in functions
- [Language specification](spec/HARN_SPEC.md) — formal spec covering lexical rules, grammar, and semantics
- [AST reference](spec/AST.md) — node types used by the parser

## For language designers

Harn is a tree-walking async interpreter written in Rust. The execution pipeline is:

```text
source → Lexer → Parser → TypeChecker → Interpreter
```

The codebase is organized as a Cargo workspace:

| Crate | Purpose |
|---|---|
| `harn-lexer` | Tokenizer |
| `harn-parser` | Recursive-descent parser, AST, static type checker |
| `harn-runtime` | Async tree-walking interpreter, values, environments |
| `harn-stdlib` | Builtins: I/O, JSON, HTTP, LLM, concurrency |
| `harn-vm` | Bytecode compiler and VM (alternative backend) |
| `harn-cli` | CLI: `run`, `test`, `repl`, `version` |
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
make conformance  # 76 language conformance tests
```

Conformance tests in `conformance/` are the primary way to verify language
behavior. Each test is a `.harn` file paired with a `.expected` or `.error`
file. Add one whenever you change the parser or interpreter.

Pre-commit hooks run `fmt` and `clippy` automatically. After cloning, set them up:

```bash
git config core.hooksPath .githooks
```
