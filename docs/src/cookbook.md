# Cookbook

Task-oriented recipes for building agents and pipelines in Harn. Each
recipe is self-contained — copy a block into a `.harn` file, set the
provider credentials it expects, and run.

For deeper topics that need their own pages, see:

- [Channel cookbook](./cookbooks/channels.md) — agent channel patterns
- [Pool cookbook](./cookbooks/pools.md) — agent pool patterns
- [Pipeline lifecycle cookbook](./cookbooks/lifecycle.md) — `on_finish`,
  handoffs, suspend/resume
- [Tool hooks cookbook](./cookbooks/tool-hooks.md) — `preset_run_command`
  recipes per stack
- [OAuth client + provider cookbook](./oauth.md) — connector OAuth
- [Extending the CLI in `.harn`](./cli-extending-in-harn.md) — add or
  port a `harn` subcommand without writing Rust

## LLM calls

### How to make a basic LLM call

Single-shot prompt with a system message. Set `ANTHROPIC_API_KEY` (or
the appropriate key for your provider) before running.

```harn
pipeline default(task) {
  let response = llm_call(
    "Explain the builder pattern in three sentences.",
    "You are a software engineering tutor. Be concise."
  )
  log(response)
}
```

To switch provider or model, pass an options dict:

```harn
pipeline default(task) {
  let response = llm_call(
    "Explain the builder pattern in three sentences.",
    "You are a software engineering tutor. Be concise.",
    {provider: "openai", model: "gpt-4o", max_tokens: 512}
  )
  log(response)
}
```

### How to ask for structured JSON output

Ask the model for JSON, parse it with `json_parse`, and validate the
shape before using it. Wrap the parse in `retry` so a malformed first
attempt does not end the run.

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
      let parsed = json_parse(raw.text)

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
  let sorted = plan.filter({ s -> s.priority <= 3 })
  for step in sorted {
    log("[P${step.priority}] ${step.step}")
  }
}
```

For schema-validated JSON without the manual guards, use
[`llm_call_structured`](./llm/llm_call.md#llm_call_structured).

### How to evaluate prompts in parallel

`parallel each` fans out a list of prompts. Results preserve the input
order.

```harn
let prompts = [
  "Explain quicksort in two sentences.",
  "Explain mergesort in two sentences.",
  "Explain heapsort in two sentences."
]

let responses = parallel each prompts { p ->
  llm_call(p, "Be concise.")
}

for r in responses {
  log(r.text)
}
```

## Tools and agent loops

### How to give an agent tools

Register tools with JSON Schema-compatible parameters, generate a system
prompt that describes them, then let the LLM call tools in a loop. For
typed tools and Tool Vault, see [LLM tools](./llm/tools.md).

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
    let calls = tool_parse_call(response.text)

    if calls.count() == 0 {
      log(response)
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

For loop-until-done agents that own completion detection and budget
enforcement, reach for [`agent_loop`](./llm/agent_loop.md) instead of
writing the loop yourself.

### How to delegate to multiple worker agents

Spawn workers for different roles and collect their results.

```harn
pipeline default(task) {
  let roles = ["research", "analyze", "summarize"]

  let results = parallel each roles { role ->
    let agent = spawn_agent({
      name: role,
      task: "Handle ${role}: ${task}",
      node: {
        kind: "subagent",
        mode: "llm",
        model_policy: {provider: "mock"},
        output_contract: {output_kinds: ["summary"]},
      },
    })
    wait_agent(agent)
  }

  for r in results {
    log(r)
  }
}
```

## Precise edits with AST tools

Agent-authored source mutations are easiest to keep correct when they speak
the language of the tree, not the language of the diff. `std/edit` ships
five primitives covering the common reaches. Pick by the **shape** of the
change first; let the language-support table tie-break:

| Shape | Reach for | Falls back to |
|---|---|---|
| Replace an existing node (function body, call expression, declaration) | [`edit_apply_node`](#how-to-rewrite-a-function-body-via-a-tree-sitter-query) | `edit_safe_text_patch` when the language has no grammar |
| Add a sibling or child next to a node (new test, new import, new arm) | [`edit_insert_at_anchor`](#how-to-add-a-new-test-to-a-rust-mod) | `edit_safe_text_patch` |
| Rename an identifier across the workspace | [`edit_rename_symbol`](#how-to-rename-a-symbol-across-the-workspace) | `edit_safe_text_patch` only when the language is out-of-batch |
| Preview a multi-step plan before committing | [`edit_dry_run`](#how-to-preview-a-multi-step-edit-plan-and-approve) | (composes with all of the above) |
| Anything else, or text-shaped change with collision risk | [`edit_safe_text_patch`](#how-to-apply-a-multi-hunk-text-patch-atomically) | — (this *is* the fallback) |

The decision is the same one a human editor makes: structural changes
through the AST, textual changes through the patch. A `system_reminder`
that lifts this table into the agent's next-turn prompt — see
[How to nudge an agent toward AST tools](#how-to-nudge-an-agent-toward-ast-tools)
— is the smallest change that durably shifts a coding agent away from
freeform text patches.

### How to rewrite a function body via a tree-sitter query

`edit_apply_node` from [`std/edit`](./stdlib/edit.md) replaces AST nodes
matched by a Tree-Sitter query with a fresh fragment, leaving the
surrounding indentation and trailing trivia untouched. This is the
right reach when an agent needs to "change this function body" or
"replace this call" — freeform text patching breaks on whitespace
drift; AST-aware splice does not.

The query must declare at least one capture; the capture named by
`target_capture` (default `target`) is the replaced span. Single-capture
queries accept any capture name.

```harn,ignore
import "std/edit"

pipeline default(task) {
  let result = edit_apply_node({
    path: "src/lib.rs",
    query: "(function_item name: (identifier) @name (#eq? @name \"greet\") body: (block) @target)",
    replacement: "{ format!(\"hi {name}!\") }",
  })

  if !result.applied {
    log("edit failed: ${result.result} — ${result.details}")
    return
  }
  log("rewrote ${len(result.edits)} match(es) in ${result.path}")
}
```

The default selector is `"unique"`: more than one match returns
`result == "ambiguous"`. Use `"first"`, `"all"`, or `"nth"` (with
`nth: N`, 1-based) to disambiguate.

```harn,ignore
import "std/edit"

pipeline default(task) {
  // Rewrite every `fn foo() { … }` body in a file.
  let result = edit_apply_node({
    path: "src/lib.rs",
    query: "(function_item body: (block) @target)",
    replacement: "{ unimplemented!() }",
    select: "all",
  })
  log("rewrote ${result.match_count} bodies")
}
```

Validation is on by default: the post-edit source is re-parsed and any
tree-sitter `ERROR` / `MISSING` node aborts with
`result == "syntax_error"`. The original file is left untouched on
rejection.

```harn,ignore
import "std/edit"

pipeline default(task) {
  let result = edit_apply_node({
    path: "src/lib.rs",
    query: "(function_item body: (block) @target)",
    replacement: "{ (",  // intentional syntax error
  })
  // result.applied == false, result.result == "syntax_error",
  // file on disk is unchanged.
  log(result.details)
}
```

Pass `dry_run: true` to inspect the splice without writing. When a
hostlib `session_id` is supplied, both the read and the write route
through the staged filesystem (see issue #1722), so the edit is atomic
alongside any sibling staged writes.

```harn,ignore
import "std/edit"

pipeline default(task) {
  let preview = edit_apply_node({
    path: "src/lib.rs",
    query: "(function_item body: (block) @target)",
    replacement: "{ 42 }",
    select: "first",
    dry_run: true,
  })
  log(preview.preview)  // rewritten source; file on disk is untouched
}
```

Supported languages on the first batch: Rust, TypeScript / TSX,
JavaScript / JSX, Python, Go, Swift, Java, C / C++, C#, Ruby, Kotlin,
PHP, Scala, Bash, Zig, Elixir, Lua, Haskell, R. Languages outside the
table return `result == "unsupported_language"`; callers can fall back
to `edit_apply_old_new_patch` (text-mode) in that branch.

### How to add a new test to a Rust mod

`edit_insert_at_anchor` (also from [`std/edit`](./stdlib/edit.md)) is
the companion primitive for adding a sibling or child node next to an
AST anchor — perfect for "append a new test case to this `mod tests`"
or "add a new variant before the trailing `}`". Where `apply_node`
*replaces* a span, `insert_at_anchor` *adds* one.

```harn,ignore
import "std/edit"

pipeline default(task) {
  // src/lib.rs contains:
  //
  //   #[cfg(test)]
  //   mod tests {
  //       #[test]
  //       fn one() {}
  //   }
  //
  let result = edit_insert_at_anchor({
    path: "src/lib.rs",
    query: "(mod_item name: (identifier) @name (#eq? @name \"tests\") body: (declaration_list) @anchor)",
    position: "last_child",
    content: "#[test]\nfn two() {}",
  })
  log(result.result)        // "applied"
  log(result.position)      // "last_child"
}
```

`position` is one of `"before"`, `"after"`, `"first_child"`, or
`"last_child"`. The first two place a sibling at the anchor's indent
depth; the last two place a child at the anchor's body depth (taken
from existing children if present, else `anchor_indent + indent_unit`).

The anchor query must match exactly one node. Multi-match returns
`result == "ambiguous"` and lists every competing span. Tighten with
`(#eq? @name "…")` or an extra structural predicate to pin a single
target.

### How to add a new import to a TypeScript file

```harn,ignore
import "std/edit"

pipeline default(task) {
  // src/index.ts contains:
  //
  //   import { a } from "./a";
  //   import { b } from "./b";
  //
  //   const x = 1;
  //
  let result = edit_insert_at_anchor({
    path: "src/index.ts",
    // Anchor on the last existing import so the new line lands right
    // below it (and above any code).
    query: "(import_statement source: (string (string_fragment) @src) (#eq? @src \"./b\")) @anchor",
    position: "after",
    content: "import { c } from \"./c\";",
  })
  log(result.applied)       // true
}
```

Validation is on by default for both primitives: the post-edit source
is re-parsed and any tree-sitter `ERROR` / `MISSING` node aborts with
`result == "syntax_error"`, leaving the file on disk untouched. Pair
either primitive with a `session_id` to route the read + write through
the staged filesystem (#1722) so the edit is atomic alongside other
staged writes.

### How to apply a multi-hunk text patch atomically

`edit_safe_text_patch` is the right reach for text edits that touch
the filesystem — it composes hunks against the staged-fs overlay,
hash-checks the pre-image, and commits all-or-nothing. Use it when
the change spans multiple regions of one file, when sibling agents
might be editing the same file in parallel, or when the language
has no tree-sitter grammar and you still need collision safety.

```harn,ignore
import { edit_safe_text_patch } from "std/edit"

pipeline default(task) {
  let snapshot = hostlib_fs_read_text({path: "src/lib.rs"})
  let result = edit_safe_text_patch({
    path: "src/lib.rs",
    expected_hash: snapshot.sha256,
    hunks: [
      {old_text: "return 1", new_text: "return 11"},
      {old_text: "return 3", new_text: "return 33"},
    ],
  })

  if result.result == "stale_base" {
    // Another writer landed first. Re-read and retry — never blind-write.
    log("retrying — current hash is now ${result.current_hash}")
    return
  }
  if result.result == "hunk_conflict" {
    log("hunk ${result.failed_hunk_index} rejected: ${result.failed_hunk_error_code}")
    return
  }
  log("applied ${result.hunks_count} hunks")
}
```

The result carries a `telemetry` envelope (`applied`, `stale_base`,
`hunk_conflict`, `no_op` counters plus `hunks`) so hosts can roll up
collision rates without log scraping.

### How to preview a multi-step edit plan and approve

When an agent needs to chain several edits — rewrite a body, rename
the function, insert a new statement — running them one at a time
risks landing the first edit before the second is validated.
`edit_dry_run` measures twice: it runs the whole plan against a
transient staged-fs overlay, renders one unified diff per touched
file, then discards the overlay. Nothing reaches disk until the
caller commits.

The diff is standard unified diff with `@@ -a,b +c,d @@` hunks, so
it round-trips through `git apply` and renders cleanly in any
reviewer UI.

```harn,ignore
import "std/edit"

pipeline default(task) {
  let bundle = edit_dry_run({
    plan: [
      {
        op: "apply_node",
        path: "src/lib.rs",
        query: "(function_item body: (block) @target)",
        replacement: "{ format!(\"hi {name}!\") }",
        select: "first",
      },
      {op: "safe_text_patch", path: "src/lib.rs", old_text: "fn greet", new_text: "fn greeter"},
    ],
  })

  if bundle.result == "ok" {
    // Surface the diff to the operator for approval. The path of
    // least resistance: pipe the bundle to a HITL reviewer, then
    // re-run the same ops without dry_run to commit them.
    for file in bundle.per_file_unified_diff {
      log("---- ${file.path} (+${file.lines_added} / -${file.lines_removed})")
      log(file.diff)
    }
  } else {
    // `partial` or `no_ops_applied` — inspect bundle.ops[i].reason
    // to learn why each op was rejected (no_match, ambiguous,
    // invalid_query, syntax_error, …).
    for op in bundle.ops {
      if !op.applied {
        log("REJECTED ${op.op}: ${op.reason} — ${op.details}")
      }
    }
  }
}
```

Plan ops share a transient staged-fs session, so the second op sees
the first op's pending write. That means several ops touching the
same file collapse to one cumulative diff, which is the form a
reviewer (human or LLM) actually wants.

### How to rename a symbol across the workspace

`edit_rename_symbol` resolves an identifier through the typed symbol
graph from [`std/code_librarian`](./stdlib/code-librarian.md) and rewrites
every identifier-context occurrence — never comments, never string
literals, never partial-name matches — across every file in scope. It
short-circuits with `result == "conflict"` if `new_name` already exists
as an identifier in any file the rename would touch, so the workspace
can't end up with two identically named definitions in the same scope.

```harn,ignore
import { edit_rename_symbol } from "std/edit"

pipeline default(task) {
  let result = edit_rename_symbol({
    symbol_ref: {name: "Widget", path: "src/lib.rs", kind: "Type"},
    new_name: "Gadget",
    scope: "workspace",
    dry_run: true,
  })

  if result.result != "applied" {
    log("rename refused: ${result.result} — ${result.details ?? \"\"}")
    return
  }
  for file in result.touched_files {
    log("${file.path}: ${len(file.edits)} edit(s)")
  }
}
```

Drop `dry_run` to actually rewrite the files; pair the call with a
`session_id` for full staged-fs atomicity across the rename plus any
sibling writes. Supported languages on the first batch: Rust,
TypeScript/TSX, JavaScript/JSX, Python, Swift, Go.

For the full result shape (`touched_files[*].edits[*]` with byte and
`(row, col)` spans, plus `conflicts[*]` shadow sites), see the deeper
[Rename a symbol across the workspace](./cookbooks/rename-symbol.md)
cookbook.

### When AST tools won't work

The AST primitives all require a tree-sitter grammar for the file's
language. They return `result == "unsupported_language"` instead of
silently mangling bytes. The supported set is intentionally
narrower than what `tree-sitter` can parse — the host vendors only
grammars that have been smoke-tested against the edit primitives.

Reach for `edit_safe_text_patch` (or the lower-level
`edit_apply_old_new_patch`) when:

- the file's language is not in the supported batch (printable list
  lives next to each primitive in [`std/edit`](./stdlib/edit.md));
- the change is purely textual — `LICENSE` headers, `CHANGELOG.md`
  entries, embedded SQL inside a string literal — and writing a
  tree-sitter query would mean writing one that matches a comment or
  string node anyway;
- the agent reasoned in terms of literal lines and the model already
  has the exact pre-image bytes in the prompt;
- a sibling agent might be rewriting the same file concurrently and
  you need a deterministic `stale_base` outcome rather than a tree
  re-parse failure.

`edit_safe_text_patch` is the safe-by-default text fallback: it
hash-checks the pre-image against `expected_hash`, composes all hunks
against the same staged-fs overlay, and either commits the post-image
atomically or returns a single rejection result that the caller can
react to.

```harn,ignore
import { edit_safe_text_patch } from "std/edit"

pipeline default(task) {
  let snapshot = hostlib_fs_read_text({path: "CHANGELOG.md"})
  let result = edit_safe_text_patch({
    path: "CHANGELOG.md",
    expected_hash: snapshot.sha256,
    hunks: [
      {old_text: "## Unreleased\n", new_text: "## Unreleased\n\n- Fixed onboarding race.\n"},
    ],
  })
  log(result.result)
}
```

The progression — try the AST primitive first, drop to
`edit_safe_text_patch` on `unsupported_language`, give up and ask the
operator only when the patch also rejects — keeps the agent on the
narrowest tool that can finish the job.

### How to nudge an agent toward AST tools

The point of these primitives is only realised when the agent *reaches*
for them. The smallest change that durably moves a coding agent away
from freeform text patches is one `system_reminder` lifted into the
prompt for every coding-agent loop. Reminders are typed, ephemeral
transcript injections with TTL and dedupe — see
[System reminders](./system-reminders.md) for the lifecycle — so the
snippet survives the next turn but does not bloat the durable transcript.

The canonical body and producer wiring:

```harn,ignore
let edit_strategy_reminder = """
Prefer the AST-precise primitives in std/edit when modifying source:
- edit_apply_node for replacing a node (function body, call, decl).
- edit_insert_at_anchor for adding a sibling/child (test, import, arm).
- edit_rename_symbol for cross-file identifier renames.
- edit_dry_run to preview a multi-op plan before committing.
Fall back to edit_safe_text_patch only when the language has no
tree-sitter grammar or the change is purely textual.
"""

let injected = transcript.inject_reminder(transcript(), {
  body: edit_strategy_reminder,
  tags: ["edit_strategy"],
  dedupe_key: "edit_strategy:prefer_ast",
  ttl_turns: 4,
  preserve_on_compact: true,
  propagate: "all",
  role_hint: "developer",
})
```

`propagate: "all"` lets sub-agents inherit the same guidance, and
`preserve_on_compact: true` keeps it visible across the next compaction
boundary. The `dedupe_key` makes the reminder safe to re-inject on
every iteration (e.g. from a session_start hook) — the lifecycle
collapses duplicates automatically.

Lift the same body into a host-side reminder by sending an ACP
`session/remind` notification with the same payload; see
[System reminders > From a host bridge](./system-reminders.md#from-a-host-bridge).

## MCP servers

### How to call an MCP server

Connect to an MCP-compatible tool server, list available tools, and call
them. This example uses the filesystem MCP server.

```harn
pipeline default(task) {
  let client = mcp_connect("npx", ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"])

  let info = mcp_server_info(client)
  log("Connected to: ${info.name}")

  let tools = mcp_list_tools(client)
  for t in tools {
    log("Tool: ${t.name} - ${t.description}")
  }

  mcp_call(client, "write_file", {path: "/tmp/hello.txt", content: "Hello from Harn!"})
  let content = mcp_call(client, "read_file", {path: "/tmp/hello.txt"})
  log("File content: ${content}")

  let entries = mcp_call(client, "list_directory", {path: "/tmp"})
  log(entries)

  mcp_disconnect(client)
}
```

You can also declare MCP servers in `harn.toml` for automatic
connection. See [MCP, ACP, and A2A integration](./mcp-and-acp.md) for the
config surface.

For remote HTTP MCP servers, authorize once with the CLI and reuse the
stored token:

```bash
harn mcp redirect-uri
harn mcp login https://mcp.notion.com/mcp --scope "read write"
```

### How to give an agent MCP tools

Connect to an MCP server and feed its tools to `agent_loop`. The LLM
chooses which to call.

```harn
pipeline default(task) {
  let client = mcp_connect("npx", ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"])
  let mcp_tool_list = mcp_list_tools(client)

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
      loop_until_done: true,
      max_iterations: 10
    }
  )

  log(result.text)
  mcp_disconnect(client)
}
```

## Concurrency

### How to run N indexed tasks in parallel

Use `parallel` when the unit of work is "N independent operations" and
you want them indexed by position.

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
    log(r)
  }
}
```

Use `parallel each` when you're mapping over a collection. See
[Concurrency](./concurrency.md) for the full surface, including
backpressure and streamed results.

### How to coordinate spawned tasks with channels

Channels coordinate producers and consumers. Use them when one spawned
task needs to hand work to another and you want backpressure rather
than an unbounded queue.

```harn
pipeline default(task) {
  let ch = channel("work", 10)
  let results_ch = channel("results", 10)

  let producer = spawn {
    let items = ["item_a", "item_b", "item_c"]
    for item in items {
      send(ch, item)
    }
    send(ch, "DONE")
  }

  let consumer = spawn {
    var processed = 0
    var running = true
    while running {
      let item = receive(ch)
      if item == "DONE" {
        running = false
      } else {
        send(results_ch, "processed: ${item}")
        processed = processed + 1
      }
    }
    send(results_ch, "COMPLETE:${processed}")
  }

  await(producer)
  await(consumer)

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

For durable cross-agent channels (publish from one pipeline, subscribe
from another), see the [Channel cookbook](./cookbooks/channels.md).

### How to recurse without blowing the stack

Tail-recursive functions are optimized by the VM, so deep recursion does
not overflow even across thousands of iterations.

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
    log("=== ${p.task} ===")
    log(p.plan)
  }
}
```

For non-LLM workloads, tail-call optimization handles deep recursion
without issue:

```harn
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

## Error handling

### How to retry and structure errors

Wrap LLM calls in `try`/`catch` with `retry` to handle transient
failures. Use a typed catch when you want different handling per error
kind.

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
        return json_parse(raw.text)
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
  } catch (e) {
    if type_of(e) == "enum" {
      match e.variant {
        "LlmFailure" -> { log("LLM failed after retries: ${e.fields[0]}") }
        "ParseFailure" -> { log("Could not parse LLM output: ${e.fields[0]}") }
        "Timeout" -> { log("Timed out after ${e.fields[0]}s") }
      }
    } else {
      log("Unexpected error: ${e}")
    }
  }
}
```

For deeper coverage of the error model, see
[Error handling](./error-handling.md).

## Patterns

### How to build context from multiple sources

Gather context in parallel, merge it into a single dict, and feed it to
the model.

```harn
pipeline default(task) {
  fn read_or_empty(path) {
    try {
      return read_file(path)
    } catch (e) {
      return ""
    }
  }

  let sources = ["README.md", "CHANGELOG.md", "docs/architecture.md"]

  let contents = parallel each sources { path ->
    {path: path, content: read_or_empty(path)}
  }

  var context = {task: task, files: {}}
  for item in contents {
    if item.content != "" {
      context = context.merge({files: context.files.merge({[item.path]: item.content})})
    }
  }

  var prompt = "Task: ${task}\n\n"
  for entry in context.files {
    prompt += "=== ${entry.key} ===\n${entry.value}\n\n"
  }

  let result = llm_call(prompt, "You are a helpful assistant. Use the provided files as context.")
  log(result)
}
```

### How to compose pipelines across files

Split logic into reusable pipelines using `import` and `extends`. See
[Modules and imports](./modules.md) for the resolution rules.

**lib/context.harn** — shared context gathering:

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

**lib/review.harn** — a reusable review pipeline:

```harn,ignore
import "lib/context"

pipeline review(task) {
  let ctx = gather_context(task)
  let prompt = "Review this project.\n\nREADME:\n${ctx.readme}\n\nTask: ${ctx.task}"
  let result = llm_call(prompt, "You are a code reviewer.")
  log(result)
}
```

**main.harn** — extend and customize:

```harn,ignore
import "lib/review"

pipeline default(task) extends review {
  override setup() {
    log("Starting custom review pipeline")
  }
}
```

### How to filter with `in` and `not in`

`in` works on lists, strings (substring test), dicts (key membership),
and sets.

```harn
pipeline default(task) {
  let allowed_extensions = [".rs", ".harn", ".toml"]
  let files = list_dir("src")

  let relevant = files.filter({ f ->
    let ext = extname(f)
    ext in allowed_extensions
  })

  log("Relevant files: ${relevant}")

  let config = {host: "localhost", port: 8080, debug: true, secret: "abc"}
  let sensitive = ["secret", "password"]

  for entry in config {
    if entry.key not in sensitive {
      log("${entry.key}: ${entry.value}")
    }
  }
}
```

### How to deduplicate with sets

Sets give O(1)-style membership testing and are immutable —
`set_add` returns a new set rather than mutating in place.

```harn
pipeline default(task) {
  let urls = [
    "https://example.com/a",
    "https://example.com/b",
    "https://example.com/a",
    "https://example.com/c",
    "https://example.com/b"
  ]

  let unique_urls = to_list(set(urls))
  log("${len(unique_urls)} unique URLs out of ${len(urls)} total")

  var visited = set()
  for url in unique_urls {
    if !set_contains(visited, url) {
      log("Processing: ${url}")
      visited = set_add(visited, url)
    }
  }

  let batch_a = set("task-1", "task-2", "task-3")
  let batch_b = set("task-2", "task-3", "task-4")

  let already_done = set_intersect(batch_a, batch_b)
  let new_work = set_difference(batch_b, batch_a)

  log("Overlap: ${len(already_done)}, New: ${len(new_work)}")
}
```

### How to enforce types at call boundaries

Annotate function parameters. The VM throws a `TypeError` before the
body runs if a caller passes the wrong type; `harn check` rejects most
of these statically.

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

  log(summarize("The quick brown fox jumps over the lazy dog", 5))

  try {
    summarize(42, "not a number")
  } catch (e) {
    log("Caught: ${e}")
    // -> TypeError: parameter 'text' expected string, got int (42)
  }

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

Annotations cover `string`, `int`, `float`, `bool`, `list`, `dict`, and
`set`. For structural shape types, see [Language basics](./language-basics.md#types-and-values).

### How to track quality metrics

`eval_metric` records named values to the run record for later
analysis. Track accuracy, latency, token usage, and failure counts
during agent execution.

```harn
pipeline default(task) {
  let result = llm_call(task, "Be concise.")
  let usage = llm_usage()

  eval_metric("cost_tokens", usage.input_tokens + usage.output_tokens)
  eval_metric("output_length", result.text.length, {model: result.model})
  log(result.text)
}
```

See [Debugging agent runs](./debugging.md#evaluation) for the eval
harness around these calls.
