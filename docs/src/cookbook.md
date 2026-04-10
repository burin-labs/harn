# Cookbook

Practical patterns for building agents and pipelines in Harn. Each
recipe is self-contained with a short explanation and working code.

## 1. Basic LLM call

Single-shot prompt with a system message. Set `ANTHROPIC_API_KEY` (or the
appropriate key for your provider) before running.

```harn
pipeline default(task) {
  let response = llm_call(
    "Explain the builder pattern in three sentences.",
    "You are a software engineering tutor. Be concise."
  )
  println(response)
}
```

To use a different provider or model, pass an options dict:

```harn
pipeline default(task) {
  let response = llm_call(
    "Explain the builder pattern in three sentences.",
    "You are a software engineering tutor. Be concise.",
    {provider: "openai", model: "gpt-4o", max_tokens: 512}
  )
  println(response)
}
```

## 2. Agent loop with tools

Register tools with JSON Schema-compatible definitions, generate a
system prompt that describes them, then let the LLM call tools in a loop.

```harn
pipeline default(task) {
  var tools = tool_registry()

  tools = tool_define(tools, "read", "Read a file from disk", {
    parameters: {path: {type: "string", description: "Path to read"}},
    returns: {type: "string"},
    handler: { path -> return read_file(path) }
  })

  tools = tool_define(tools, "search", "Search code for a pattern", {
    parameters: {query: {type: "string", description: "Query to search"}},
    returns: {type: "string"},
    handler: { query ->
      let result = shell("grep -r '${query}' src/ || true")
      return result.stdout
    }
  })

  let system = tool_prompt(tools)

  var messages = task
  var done = false
  var iterations = 0

  while !done && iterations < 10 {
    let response = llm_call(messages, system)
    let calls = tool_parse_call(response)

    if calls.count() == 0 {
      println(response)
      done = true
    } else {
      var tool_output = ""
      for call in calls {
        let t = tool_find(tools, call.name)
        let handler = t.handler
        let result = handler(call.arguments[call.arguments.keys()[0]])
        tool_output = tool_output + tool_format_result(call.name, result)
      }
      messages = tool_output
    }
    iterations = iterations + 1
  }
}
```

## 3. Parallel tool execution

Run multiple independent operations concurrently with `parallel each`.
Results preserve the original list order.

```harn
pipeline default(task) {
  let files = ["src/main.rs", "src/lib.rs", "src/utils.rs"]

  let reviews = parallel each files { file ->
    let content = read_file(file)
    llm_call(
      "Review this code for bugs and suggest fixes:\n\n${content}",
      "You are a senior code reviewer. Be specific."
    )
  }

  for i in 0 upto files.count {
    println("=== ${files[i]} ===")
    println(reviews[i])
  }
}
```

Use `parallel` when you need to run N indexed tasks rather than mapping
over a list:

```harn
pipeline default(task) {
  let prompts = [
    "Write a haiku about Rust",
    "Write a haiku about concurrency",
    "Write a haiku about debugging"
  ]

  let results = parallel(prompts.count) { i ->
    llm_call(prompts[i], "You are a poet.")
  }

  for r in results {
    println(r)
  }
}
```

## 4. MCP client integration

Connect to an MCP-compatible tool server, list available tools, and call
them. This example uses the filesystem MCP server.

```harn
pipeline default(task) {
  let client = mcp_connect("npx", ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"])

  // Check connection
  let info = mcp_server_info(client)
  println("Connected to: ${info.name}")

  // List available tools
  let tools = mcp_list_tools(client)
  for t in tools {
    println("Tool: ${t.name} - ${t.description}")
  }

  // Write a file, then read it back
  mcp_call(client, "write_file", {path: "/tmp/hello.txt", content: "Hello from Harn!"})
  let content = mcp_call(client, "read_file", {path: "/tmp/hello.txt"})
  println("File content: ${content}")

  // List directory
  let entries = mcp_call(client, "list_directory", {path: "/tmp"})
  println(entries)

  mcp_disconnect(client)
}
```

You can also declare MCP servers in `harn.toml` for automatic connection.
See [MCP and ACP Integration](./mcp-and-acp.md) for details.

For remote HTTP MCP servers, authorize once with the CLI and then reuse the
stored token automatically from `harn.toml`:

```bash
harn mcp redirect-uri
harn mcp login https://mcp.notion.com/mcp --scope "read write"
```

## 5. Filtering with `in` and `not in`

Use the `in` and `not in` operators to filter collections by membership.

```harn
pipeline default(task) {
  let allowed_extensions = [".rs", ".harn", ".toml"]
  let files = list_dir("src")

  // Filter files to only allowed extensions
  let relevant = files.filter({ f ->
    let ext = extname(f)
    ext in allowed_extensions
  })

  println("Relevant files: ${relevant}")

  // Exclude specific keys from a config dict
  let config = {host: "localhost", port: 8080, debug: true, secret: "abc"}
  let sensitive = ["secret", "password"]

  let safe = {}
  for entry in config {
    if entry.key not in sensitive {
      println("${entry.key}: ${entry.value}")
    }
  }
}
```

The `in` operator works with lists, strings (substring test), dicts
(key membership), and sets.

## 6. Pipeline composition

Split agent logic across files and compose pipelines using imports
and inheritance.

**lib/context.harn** -- shared context-gathering logic:

```harn
fn gather_context(task) {
  let readme = read_file("README.md")
  return {
    task: task,
    readme: readme,
    timestamp: timestamp()
  }
}
```

**lib/review.harn** -- a reusable review pipeline:

```harn,ignore
import "lib/context"

pipeline review(task) {
  let ctx = gather_context(task)
  let prompt = "Review this project.\n\nREADME:\n${ctx.readme}\n\nTask: ${ctx.task}"
  let result = llm_call(prompt, "You are a code reviewer.")
  println(result)
}
```

**main.harn** -- extend and customize:

```harn,ignore
import "lib/review"

pipeline default(task) extends review {
  override setup() {
    println("Starting custom review pipeline")
  }
}
```

## 7. Error handling in agent loops

Wrap LLM calls in `try`/`catch` with `retry` to handle transient failures.
Use typed catch for structured error handling.

```harn
pipeline default(task) {
  enum AgentError {
    LlmFailure(message)
    ParseFailure(raw)
    Timeout(seconds)
  }

  fn safe_llm_call(prompt, system) {
    retry 3 {
      try {
        let raw = llm_call(prompt, system)
        let parsed = json_parse(raw)
        return parsed
      } catch (e) {
        println("LLM call failed: ${e}")
        throw AgentError.LlmFailure(to_string(e))
      }
    }
  }

  try {
    let result = safe_llm_call(
      "Return a JSON object with keys 'summary' and 'score'.",
      "You are an evaluator. Always respond with valid JSON only."
    )
    println("Summary: ${result.summary}")
    println("Score: ${result.score}")
  } catch (e) {
    // Harn supports a single catch per try; branch on the error type here.
    if type_of(e) == "enum" {
      match e.variant {
        "LlmFailure" -> { println("LLM failed after retries: ${e.fields[0]}") }
        "ParseFailure" -> { println("Could not parse LLM output: ${e.fields[0]}") }
        "Timeout" -> { println("Timed out after ${e.fields[0]}s") }
      }
    } else {
      println("Unexpected error: ${e}")
    }
  }
}
```

## 8. Channel-based coordination

Use channels to coordinate between spawned tasks. One task produces work,
another consumes it.

```harn
pipeline default(task) {
  let ch = channel("work", 10)
  let results_ch = channel("results", 10)

  // Producer: send work items
  let producer = spawn {
    let items = ["item_a", "item_b", "item_c"]
    for item in items {
      send(ch, item)
    }
    send(ch, "DONE")
  }

  // Consumer: process work items
  let consumer = spawn {
    var processed = 0
    var running = true
    while running {
      let item = receive(ch)
      if item == "DONE" {
        running = false
      } else {
        let result = "processed: ${item}"
        send(results_ch, result)
        processed = processed + 1
      }
    }
    send(results_ch, "COMPLETE:${processed}")
  }

  await(producer)
  await(consumer)

  // Collect results
  var collecting = true
  while collecting {
    let msg = receive(results_ch)
    if msg.starts_with("COMPLETE:") {
      println(msg)
      collecting = false
    } else {
      println(msg)
    }
  }
}
```

## 9. Context building pattern

Gather context from multiple sources, merge it into a single dict, and
pass it to an LLM.

```harn
pipeline default(task) {
  fn read_or_empty(path) {
    try {
      return read_file(path)
    } catch (e) {
      return ""
    }
  }

  // Gather context from multiple sources in parallel
  let sources = ["README.md", "CHANGELOG.md", "docs/architecture.md"]

  let contents = parallel each sources { path ->
    {path: path, content: read_or_empty(path)}
  }

  // Build a merged context dict
  var context = {task: task, files: {}}
  for item in contents {
    if item.content != "" {
      context = context.merge({files: context.files.merge({[item.path]: item.content})})
    }
  }

  // Format context for the LLM
  var prompt = "Task: ${task}\n\n"
  for entry in context.files {
    prompt += "=== ${entry.key} ===\n${entry.value}\n\n"
  }

  let result = llm_call(prompt, "You are a helpful assistant. Use the provided files as context.")
  println(result)
}
```

## 10. Structured output parsing

Ask the LLM for JSON output, parse it with `json_parse`, and validate
the structure before using it.

```harn
pipeline default(task) {
  let system = """
You are a task planner. Given a task description, break it into steps.
Respond with ONLY a JSON array of objects, each with "step" (string) and
"priority" (int 1-5). No other text.
"""

  fn get_plan(task_desc) {
    retry 3 {
      let raw = llm_call(task_desc, system)
      let parsed = json_parse(raw)

      // Validate structure
      guard type_of(parsed) == "list" else {
        throw "Expected a JSON array, got: ${type_of(parsed)}"
      }

      for item in parsed {
        guard item.has("step") && item.has("priority") else {
          throw "Missing required fields in: ${json_stringify(item)}"
        }
      }

      return parsed
    }
  }

  let plan = get_plan("Build a REST API for a todo app")

  if plan != nil {
    let sorted = plan.filter({ s -> s.priority <= 3 })
    for step in sorted {
      println("[P${step.priority}] ${step.step}")
    }
  } else {
    println("Failed to get a valid plan after retries")
  }
}
```

## 11. Sets for deduplication and membership testing

Use sets to track processed items and avoid duplicates. Sets provide
O(1)-style membership testing via `set_contains` and are immutable --
operations like `set_add` return a new set.

```harn
pipeline default(task) {
  let urls = [
    "https://example.com/a",
    "https://example.com/b",
    "https://example.com/a",
    "https://example.com/c",
    "https://example.com/b"
  ]

  // Deduplicate with set(), then convert back to a list
  let unique_urls = to_list(set(urls))
  println("${len(unique_urls)} unique URLs out of ${len(urls)} total")

  // Track which URLs have been processed
  var visited = set()

  for url in unique_urls {
    if !set_contains(visited, url) {
      println("Processing: ${url}")
      visited = set_add(visited, url)
    }
  }

  // Set operations: find overlap between two batches
  let batch_a = set("task-1", "task-2", "task-3")
  let batch_b = set("task-2", "task-3", "task-4")

  let already_done = set_intersect(batch_a, batch_b)
  let new_work = set_difference(batch_b, batch_a)

  println("Overlap: ${len(already_done)}, New: ${len(new_work)}")
}
```

## 12. Typed functions with runtime enforcement

Add type annotations to function parameters for automatic runtime
validation. When a caller passes a value of the wrong type, the VM
throws a `TypeError` before the function body executes.

```harn,ignore
pipeline default(task) {
  fn summarize(text: string, max_words: int) -> string {
    let words = text.split(" ")
    if words.count <= max_words {
      return text
    }
    let truncated = words.slice(0, max_words)
    return "${join(truncated, " ")}..."
  }

  println(summarize("The quick brown fox jumps over the lazy dog", 5))

  // Catch type errors gracefully. `harn check` rejects this call statically
  // before the catch can run — the example is shown for illustration only.
  try {
    summarize(42, "not a number")
  } catch (e) {
    println("Caught: ${e}")
    // -> TypeError: parameter 'text' expected string, got int (42)
  }

  // Works with all primitive types: string, int, float, bool, list, dict, set
  fn process_batch(items: list, verbose: bool) {
    for item in items {
      if verbose {
        println("Processing: ${item}")
      }
    }
    println("Done: ${len(items)} items")
  }

  process_batch(["a", "b", "c"], true)
}
```

## 13. MCP client with agent loop

Connect to an MCP server and pass its tools to an `agent_loop`, letting
the LLM decide which tools to call.

```harn
pipeline default(task) {
  let client = mcp_connect("npx", ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"])
  let mcp_tool_list = mcp_list_tools(client)

  // Build a tool registry from MCP tools
  var tools = tool_registry()
  for t in mcp_tool_list {
    tools = tool_define(tools, t.name, t.description, {
      parameters: t.inputSchema?.properties ?? {},
      returns: {type: "string"},
      handler: { args -> return mcp_call(client, t.name, args) }
    })
  }

  let result = agent_loop(
    "List all files in /tmp and read the first one.",
    "You are a helpful file assistant.",
    {
      tools: tools,
      persistent: true,
      max_iterations: 10
    }
  )

  println(result.text)
  mcp_disconnect(client)
}
```

## 14. Recursive agent with tail call optimization

Tail-recursive functions are optimized by the VM, so they do not overflow
the stack even across thousands of iterations. This is an advanced pattern
useful for processing a queue of work items one at a time.

```harn
pipeline default(task) {
  let items = ["Refactor auth module", "Add input validation", "Write unit tests"]

  fn process(remaining, results) {
    if remaining.count == 0 {
      return results
    }
    let item = remaining.first
    let rest = remaining.slice(1)

    let result = retry 3 {
      llm_call(
        "Plan how to: ${item}",
        "You are a senior engineer. Output a numbered list of steps."
      )
    }

    return process(rest, results + [{task: item, plan: result}])
  }

  let plans = process(items, [])

  for p in plans {
    println("=== ${p.task} ===")
    println(p.plan)
  }
}
```

For non-LLM workloads, TCO handles deep recursion without issues:

```harn
pipeline default(task) {
  fn sum_to(n, acc) {
    if n <= 0 {
      return acc
    }
    return sum_to(n - 1, acc + n)
  }

  println(sum_to(10000, 0))
}
```

## 15. Multi-agent delegation

Spawn worker agents for different roles and collect their results in
parallel.

```harn
// Spawn workers and collect results
let agents = ["research", "analyze", "summarize"]
let results = parallel each agents { role ->
  let agent = spawn_agent({name: role, system: "You are a ${role} agent."})
  send_input(agent, task)
  wait_agent(agent)
}
```

## 16. Parallel LLM evaluation

Evaluate multiple prompts concurrently using `parallel each`.

```harn
// Evaluate multiple prompts in parallel
let prompts = ["Explain X", "Explain Y", "Explain Z"]
let responses = parallel each prompts { p ->
  llm_call({prompt: p})
}
```

## 17. MCP client usage

Connect to an MCP server, list tools, call one, and disconnect.

```harn
// Connect to an MCP server and call tools
let client = mcp_connect({command: "npx", args: ["-y", "some-mcp-server"]})
let tools = mcp_list_tools(client)
log("Available: ${len(tools)} tools")
let result = mcp_call(client, "tool_name", {arg: "value"})
mcp_disconnect(client)
```

## 18. Eval metrics tracking

Track quality metrics during agent execution for later analysis.

```harn
// Track quality metrics during agent execution
eval_metric("accuracy", score, {model: model_name})
let usage = llm_usage()
eval_metric("cost_tokens", usage.input_tokens + usage.output_tokens)
```
