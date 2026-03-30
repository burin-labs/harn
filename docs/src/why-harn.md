# Why Harn?

## The problem

Building AI agents is complex. A typical agent needs to call LLMs, execute
tools, handle errors and retries, run tasks concurrently, maintain
conversation state, and coordinate multiple sub-agents. In most languages,
this means assembling a tower of libraries:

- An LLM SDK (LangChain, OpenAI SDK, Anthropic SDK)
- An async runtime (asyncio, Tokio, goroutines)
- Retry and timeout logic (tenacity, custom decorators)
- Tool registration and dispatch (custom JSON Schema plumbing)
- Structured logging and tracing (separate packages)
- A test framework (pytest, Jest)

Each layer adds configuration, boilerplate, and failure modes.
The orchestration logic -- the part that actually matters -- gets buried
under infrastructure code.

## What Harn does differently

Harn is a programming language where agent orchestration primitives are
built into the syntax, not bolted on as libraries.

### Pipelines are the unit of composition

Every Harn program is a set of named pipelines. Pipelines can extend
each other, override steps, and be imported across files. This gives you
a natural way to structure multi-stage agent workflows:

```javascript
pipeline analyze(task) {
  let context = read_file("README.md")
  let plan = llm_call(task + "\n\nContext:\n" + context, "Break this into steps.")
  let steps = json_parse(plan)

  let results = parallel_map(steps) { step ->
    agent_loop(step, "You are a coding assistant.", {persistent: true})
  }

  write_file("results.json", json_stringify(results))
}
```

### LLM calls are builtins

`llm_call` and `agent_loop` are language primitives. No SDK imports, no
client initialization, no response parsing. Set an environment variable
and call a model:

```javascript
let answer = llm_call("Summarize this code", "You are a code reviewer.")
```

Harn supports Anthropic, OpenAI, Ollama, and OpenRouter. Switching
providers is a one-field change in the options dict.

### Native concurrency without async/await

`parallel_map`, `parallel`, `spawn`/`await`, and channels are keywords,
not library functions. No callback chains, no promise combinators, no
`async def` annotations:

```javascript
let results = parallel_map(files) { file ->
  llm_call(read_file(file), "Review this file for security issues")
}
```

### Retry and error recovery are syntax

`retry` and `try`/`catch` are control flow constructs. Wrapping an
unreliable LLM call in retries is a one-liner:

```javascript
retry 3 {
  let result = llm_call(prompt, system)
  json_parse(result)
}
```

### MCP for external tools

Harn has built-in support for the
[Model Context Protocol](https://modelcontextprotocol.io). Connect to any
MCP-compatible tool server, list its tools, and call them -- all from
within a pipeline:

```javascript
let client = mcp_connect("npx", ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"])
let tools = mcp_list_tools(client)
let content = mcp_call(client, "read_file", {path: "/tmp/data.txt"})
mcp_disconnect(client)
```

### Tail call optimization for agent loops

Recursive agent patterns -- where an agent processes one item, then calls
itself with the next -- are idiomatic in Harn. The VM performs tail call
optimization so recursive loops do not overflow the stack, even across
thousands of iterations:

```javascript
fn process_items(items, results) {
  if items.count == 0 {
    return results
  }
  let item = items.first
  let rest = items.slice(1)
  let result = llm_call(item, "Process this item")
  return process_items(rest, results + [result])
}
```

### Gradual typing

Type annotations are optional. Add them where they help, leave them off
where they don't:

```javascript
fn score(text: string) -> int {
  let result = llm_call(text, "Rate 1-10. Respond with just the number.")
  return to_int(result)
}
```

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

llm = ChatAnthropic(model="claude-sonnet-4-20250514")

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
        print(r)

asyncio.run(main())
```

**Harn**:

```javascript
pipeline default(task) {
  let urls = ["https://a.com", "https://b.com", "https://c.com"]

  let results = parallel_map(urls) { url ->
    retry 3 {
      let page = http_get(url)
      llm_call("Summarize:\n" + page, "Be concise.")
    }
  }

  for r in results {
    log(r)
  }
}
```

The Harn version has no imports, no decorators, no client initialization,
no async annotations, and no runtime setup. The orchestration logic is
all that remains.

## Getting started

Install Harn and create a project:

```bash
curl -fsSL https://raw.githubusercontent.com/burin-labs/harn/main/install.sh | sh
harn init my-agent
cd my-agent
harn run main.harn
```

See the [cookbook](cookbook.md) for practical patterns, or
[language basics](language-basics.md) for a full syntax guide.
