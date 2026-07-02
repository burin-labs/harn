<!-- markdownlint-disable MD033 -->

# Why Harn?

## The problem

Building AI agents usually means coordinating models, tools, retries,
concurrency, state, and sub-agents. In most languages, that turns into a
stack of libraries:

- An LLM SDK (LangChain, OpenAI SDK, Anthropic SDK)
- An async runtime (asyncio, Tokio, goroutines)
- Retry and timeout logic (tenacity, custom decorators)
- Tool registration and dispatch (custom JSON Schema plumbing)
- Structured logging and tracing (separate packages)
- A test framework (pytest, Jest)

Each layer adds configuration, boilerplate, and failure modes. The
orchestration logic gets buried under infrastructure code.

## What Harn does differently

Harn puts agent orchestration primitives in the language instead of leaving
them to framework glue.

For a capability-by-capability comparison with Inngest, Temporal, LangGraph,
and Cursor Automations, see the [feature matrix](feature-matrix.md).

In practice, Harn is the orchestration boundary between product code and
provider/runtime code. Product integrations declare workflows, policies,
capabilities, and UI hooks; Harn handles transcripts, tool queues, replay
fixtures, and provider response normalization.

### Native LLM calls

`llm_call` and `agent_loop` are language primitives. No SDK imports, no
client initialization, no response parsing. Set an environment variable
and call a model:

```harn
let answer = llm_call("Summarize this code", "You are a code reviewer.")
```

Harn ships with built-in configs for Anthropic, OpenAI, OpenRouter,
HuggingFace, Ollama, and local OpenAI-compatible servers. Switching
providers is a one-field change in the options dict.

### Pipeline composition

Pipelines are the unit of composition. They can extend each other, override
steps, and be imported across files, which keeps multi-stage agent workflows
readable:

```harn
pipeline analyze(task) {
  let context = read_file("README.md")
  let plan = llm_call("${task}\n\nContext:\n${context}", "Break this into steps.")
  let steps = json_parse(plan.text)

  let results = parallel each steps { step ->
    agent_loop(step, "You are a coding assistant.", {loop_until_done: true})
  }

  write_file("results.json", json_stringify(results))
}
```

Files can also contain top-level code without a pipeline block (implicit
pipeline), which keeps scripts and quick experiments short.

### MCP and ACP integration

Harn has built-in support for the
[Model Context Protocol](https://modelcontextprotocol.io). Connect to any
MCP server, or expose your Harn pipeline as one. ACP integration lets
editors use Harn as an agent backend.

The CLI handles standalone OAuth for remote HTTP MCP servers, so cloud MCP
integrations can be ordinary runtime dependencies instead of host-specific glue.

```harn
let client = mcp_connect("npx", ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"])
let tools = mcp_list_tools(client)
let content = mcp_call(client, "read_file", {path: "/tmp/data.txt"})
mcp_disconnect(client)
```

### Concurrency without async/await

Agent work is mostly waiting: for files, HTTP calls, tool calls, model
responses, or other workers. Harn makes that waiting explicit in the program
without asking you to write an event loop.

```harn
let results = parallel each files { file ->
  llm_call(read_file(file), "Review this file for security issues")
}
```

<details>
<summary>How to read this snippet</summary>

For each path in `files`, Harn starts a child task, reads the file inside that
task, calls the model, and returns the results in input order. If there are `N`
files and the slowest branch takes `T` seconds, wall-clock time is roughly `T`
plus scheduling overhead. Memory grows with in-flight tasks and their
inputs/results, so use `with { max_concurrent: K }` for large file sets or
provider queues.

Because `parallel each` is part of the language runtime, cancellation, replay,
trace spans, and host-capability checks stay attached to the whole fan-out.

</details>

### Retry and error recovery

`retry` and `try`/`catch` are control flow constructs. Wrapping an
unreliable LLM call in retries is a one-liner:

```harn
retry 3 {
  let result = llm_call(prompt, system)
  json_parse(result.text)
}
```

### Gradual typing

Type annotations are optional. Add them where they help, leave them off
where they don't. Structural shape types let you describe expected dict
fields:

```harn
type Review = {
  path: string,
  risk: "low" | "medium" | "high",
  summary?: string,
}

fn render_review(review: Review) -> string {
  return "${review.path}: ${review.risk}"
}

render_review({path: "src/auth.rs", risk: "high", owner: "security"})
```

`path` and `risk` are required keys. `summary` is optional. Extra keys such as
`owner` are allowed, so typed boundaries can describe the fields a function
actually needs without forcing every caller to erase useful metadata.

### Embeddable

Harn compiles to a WASM target for browser embedding and ships with
LSP and DAP servers for IDE integration. Agent pipelines can run inside
editors, CI systems, or web applications.

## Who Harn is for

- **Developers building AI agents** who want orchestration logic to be
  readable and concise, not buried under framework boilerplate.
- **IDE authors** who want a scriptable, embeddable language for agent
  pipelines with built-in LSP support.
- **Researchers** prototyping agent architectures who need fast iteration
  without setting up infrastructure.

## Comparison

Here is what a "fetch three URLs in parallel, summarize each with an LLM,
and retry failures" pattern looks like across approaches:

**Python (LangChain + asyncio)**:

```python
import asyncio
from langchain_anthropic import ChatAnthropic
from tenacity import retry, stop_after_attempt
import aiohttp

llm = ChatAnthropic(model="claude-sonnet-5")

@retry(stop=stop_after_attempt(3))
async def summarize(url):
    async with aiohttp.ClientSession() as session:
        async with session.get(url) as resp:
            text = await resp.text()
    result = await llm.ainvoke(f"Summarize:\n{text}")
    return result.content

async def main():
    urls = ["https://a.com", "https://b.com", "https://c.com"]
    results = await asyncio.gather(*[summarize(u) for u in urls])
    for r in results:
        log(r)

asyncio.run(main())
```

**Harn**:

```harn
pipeline default(task) {
  let urls = ["https://a.com", "https://b.com", "https://c.com"]

  let results = parallel each urls { url ->
    retry 3 {
      let page = http_get(url)
      llm_call("Summarize:\n${page}", "Be concise.")
    }
  }

  for r in results {
    log(r)
  }
}
```

The Harn version has no imports, decorators, client initialization, async
annotations, or runtime setup.

## Getting started

See the [Getting started](getting-started.md) guide to install Harn and
run your first program, or jump to the [cookbook](cookbook.md) for
practical patterns.
