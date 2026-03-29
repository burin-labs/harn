# Harn Cookbook

Practical patterns for building AI agents and pipelines in Harn. Each
recipe is self-contained with a short explanation and working code.

## 1. Basic LLM call

Single-shot prompt with a system message. Set `ANTHROPIC_API_KEY` (or the
appropriate key for your provider) before running.

```javascript
pipeline default(task) {
  let response = llm_call(
    "Explain the builder pattern in three sentences.",
    "You are a software engineering tutor. Be concise."
  )
  log(response)
}
```

To use a different provider or model, pass an options dict:

```javascript
pipeline default(task) {
  let response = llm_call(
    "Explain the builder pattern in three sentences.",
    "You are a software engineering tutor. Be concise.",
    {provider: "openai", model: "gpt-4o", max_tokens: 512}
  )
  log(response)
}
```

## 2. Agent loop with tools

Register tools with JSON Schema-compatible definitions, generate a
system prompt that describes them, then let the LLM call tools in a loop.

```javascript
pipeline default(task) {
  let tools = tool_registry()

  let tools = tool_add(tools, "read", "Read a file from disk", { path ->
    return read_file(path)
  }, {path: "string"})

  let tools = tool_add(tools, "search", "Search code for a pattern", { query ->
    let result = shell("grep -r '" + query + "' src/ || true")
    return result.stdout
  }, {query: "string"})

  let system = tool_prompt(tools)

  var messages = task
  var done = false
  var iterations = 0

  while !done && iterations < 10 {
    let response = llm_call(messages, system)
    let calls = tool_parse_call(response)

    if calls.count() == 0 {
      log(response)
      done = true
    } else {
      var tool_output = ""
      for call in calls {
        let tool = tool_find(tools, call.name)
        let handler = tool.handler
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

Run multiple independent operations concurrently with `parallel_map`.
Results preserve the original list order.

```javascript
pipeline default(task) {
  let files = ["src/main.rs", "src/lib.rs", "src/utils.rs"]

  let reviews = parallel_map(files) { file ->
    let content = read_file(file)
    llm_call(
      "Review this code for bugs and suggest fixes:\n\n" + content,
      "You are a senior code reviewer. Be specific."
    )
  }

  for i in 0 upto files.count {
    log("=== ${files[i]} ===")
    log(reviews[i])
  }
}
```

Use `parallel` when you need to run N indexed tasks rather than mapping
over a list:

```javascript
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
    log(r)
  }
}
```

## 4. MCP server integration

Connect to an MCP-compatible tool server, list available tools, and call
them. This example uses the filesystem MCP server.

```javascript
pipeline default(task) {
  let client = mcp_connect("npx", ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"])

  // Check connection
  let info = mcp_server_info(client)
  log("Connected to: ${info.name}")

  // List available tools
  let tools = mcp_list_tools(client)
  for tool in tools {
    log("Tool: ${tool.name} - ${tool.description}")
  }

  // Write a file, then read it back
  mcp_call(client, "write_file", {path: "/tmp/hello.txt", content: "Hello from Harn!"})
  let content = mcp_call(client, "read_file", {path: "/tmp/hello.txt"})
  log("File content: ${content}")

  // List directory
  let entries = mcp_call(client, "list_directory", {path: "/tmp"})
  log(entries)

  mcp_disconnect(client)
}
```

## 5. Recursive agent with TCO

Tail-recursive functions are optimized by the VM, so they do not overflow
the stack even across thousands of iterations. This pattern is useful for
processing a queue of work items one at a time.

```javascript
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
        "Plan how to: " + item,
        "You are a senior engineer. Output a numbered list of steps."
      )
    }

    return process(rest, results + [{task: item, plan: result}])
  }

  let plans = process(items, [])

  for p in plans {
    log("=== ${p.task} ===")
    log(p.plan)
  }
}
```

For non-LLM workloads, TCO handles deep recursion without issues:

```javascript
pipeline default(task) {
  fn sum_to(n, acc) {
    if n <= 0 {
      return acc
    }
    return sum_to(n - 1, acc + n)
  }

  log(sum_to(10000, 0))
}
```

## 6. Pipeline composition

Split agent logic across files and compose pipelines using imports
and inheritance.

**lib/context.harn** -- shared context-gathering logic:

```javascript
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

```javascript
import "lib/context"

pipeline review(task) {
  let ctx = gather_context(task)
  let prompt = "Review this project.\n\nREADME:\n" + ctx.readme + "\n\nTask: " + ctx.task
  let result = llm_call(prompt, "You are a code reviewer.")
  log(result)
}
```

**main.harn** -- extend and customize:

```javascript
import "lib/review"

pipeline default(task) extends review {
  override fn setup() {
    log("Starting custom review pipeline")
  }
}
```

## 7. Error handling in agent loops

Wrap LLM calls in `try`/`catch` with `retry` to handle transient failures.
Use typed catch for structured error handling.

```javascript
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
        log("LLM call failed: ${e}")
        throw AgentError.LlmFailure(to_string(e))
      }
    }
  }

  try {
    let result = safe_llm_call(
      "Return a JSON object with keys 'summary' and 'score'.",
      "You are an evaluator. Always respond with valid JSON only."
    )
    log("Summary: ${result.summary}")
    log("Score: ${result.score}")
  } catch (e: AgentError) {
    match e.variant {
      "LlmFailure" -> { log("LLM failed after retries: ${e.fields[0]}") }
      "ParseFailure" -> { log("Could not parse LLM output: ${e.fields[0]}") }
    }
  } catch (e) {
    log("Unexpected error: ${e}")
  }
}
```

## 8. Channel-based coordination

Use channels to coordinate between spawned tasks. One task produces work,
another consumes it.

```javascript
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
        let result = "processed: " + item
        send(results_ch, result)
        processed = processed + 1
      }
    }
    send(results_ch, "COMPLETE:" + to_string(processed))
  }

  await(producer)
  await(consumer)

  // Collect results
  var collecting = true
  while collecting {
    let msg = receive(results_ch)
    if msg.starts_with("COMPLETE:") {
      log(msg)
      collecting = false
    } else {
      log(msg)
    }
  }
}
```

## 9. Context building pattern

Gather context from multiple sources, merge it into a single dict, and
pass it to an LLM.

```javascript
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

  let contents = parallel_map(sources) { path ->
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
  var prompt = "Task: " + task + "\n\n"
  for entry in context.files {
    prompt = prompt + "=== " + entry.key + " ===\n" + entry.value + "\n\n"
  }

  let result = llm_call(prompt, "You are a helpful assistant. Use the provided files as context.")
  log(result)
}
```

## 10. Structured output parsing

Ask the LLM for JSON output, parse it with `json_parse`, and validate
the structure before using it.

```javascript
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
        throw "Expected a JSON array, got: " + type_of(parsed)
      }

      for item in parsed {
        guard item.has("step") && item.has("priority") else {
          throw "Missing required fields in: " + json_stringify(item)
        }
      }

      return parsed
    }
  }

  let plan = get_plan("Build a REST API for a todo app")

  if plan != nil {
    let sorted = plan.filter({ s -> s.priority <= 3 })
    for step in sorted {
      log("[P${step.priority}] ${step.step}")
    }
  } else {
    log("Failed to get a valid plan after retries")
  }
}
```

## 11. Sets for deduplication and membership testing

Use sets to track processed items and avoid duplicates. Sets provide
O(1)-style membership testing via `set_contains` and are immutable --
operations like `set_add` return a new set.

```javascript
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
  log("${len(unique_urls)} unique URLs out of ${len(urls)} total")

  // Track which URLs have been processed
  var visited = set()

  for url in unique_urls {
    if !set_contains(visited, url) {
      log("Processing: ${url}")
      visited = set_add(visited, url)
    }
  }

  // Set operations: find overlap between two batches
  let batch_a = set("task-1", "task-2", "task-3")
  let batch_b = set("task-2", "task-3", "task-4")

  let already_done = set_intersect(batch_a, batch_b)
  let new_work = set_difference(batch_b, batch_a)

  log("Overlap: ${len(already_done)}, New: ${len(new_work)}")
}
```

## 12. Typed functions with runtime enforcement

Add type annotations to function parameters for automatic runtime
validation. When a caller passes a value of the wrong type, the VM
throws a `TypeError` before the function body executes.

```javascript
pipeline default(task) {
  fn summarize(text: string, max_words: int) -> string {
    let words = text.split(" ")
    if words.count <= max_words {
      return text
    }
    let truncated = words.slice(0, max_words)
    return join(truncated, " ") + "..."
  }

  log(summarize("The quick brown fox jumps over the lazy dog", 5))

  // Catch type errors gracefully
  try {
    summarize(42, "not a number")
  } catch (e) {
    log("Caught: ${e}")
    // -> TypeError: parameter 'text' expected string, got int (42)
  }

  // Works with all primitive types: string, int, float, bool, list, dict, set
  fn process_batch(items: list, verbose: bool) {
    for item in items {
      if verbose {
        log("Processing: ${item}")
      }
    }
    log("Done: ${len(items)} items")
  }

  process_batch(["a", "b", "c"], true)
}
```
