# Tutorial: Build a code review agent

This tutorial shows a small but realistic review pipeline. The goal is not to
rebuild a full IDE integration. Instead, we want a deterministic Harn program
that can review a patch, inspect context, and return a concise report.

Use the companion example as a starting point:

```bash
cargo run --bin harn -- run examples/code-reviewer.harn
```

## 1. Start with a tight review prompt

The simplest useful reviewer is just an LLM call with a strong system prompt.
Keep the instructions short, specific, and opinionated:

```harn
pipeline default(task) {
  let system = """
You are a senior code reviewer.
Review the patch for correctness, security, maintainability, and tests.
Return:
- must-fix issues
- suggestions
- missing tests
End with a short verdict.
"""

  let review = llm_call(task, system, {
    temperature: 0.2,
    max_tokens: 1200,
  })

  println(review.text)
}
```

This is enough when the user pastes a diff directly into `task`.

## 2. Add file context when you need it

Real review agents usually need a bit of surrounding code. The simplest route
is to read a small, explicit list of files and combine them with the patch.
Keep the list short so the prompt stays focused.

```harn
pipeline default(task) {
  let files = ["src/main.rs", "src/lib.rs"]
  var context = ""

  for file in files {
    context = context + "\n\n=== " + file + " ===\n" + read_file(file)
  }

  let review = llm_call(
    "Patch:\n" + task + "\n\nContext:\n" + context,
    """
You are a strict code reviewer.
Flag correctness bugs first, then test gaps, then maintainability issues.
Do not invent missing context. If the context is insufficient, say so.
""",
    {temperature: 0.2, max_tokens: 1400}
  )

  println(review.text)
}
```

If you want to review a directory tree instead, use `list_dir()` and
`parallel each` to gather files concurrently, then trim the result to the most
relevant ones before calling the model.

## 3. Make the review measurable

Good review agents should record something observable, even if it is only a
small heuristic. Use `eval_metric()` to track whether the agent found issues
and how often it asked for more context.

```harn
pipeline default(task) {
  let review = llm_call(
    task,
    "You are a code reviewer. Return a concise bullet list.",
    {temperature: 0.2}
  )

  let has_issue = review.text.contains("issue") || review.text.contains("bug")
  eval_metric("review_has_issue", has_issue)
  eval_metric("review_chars", review.text.count)

  println(review.text)
}
```

That makes the output easier to compare in `harn eval` runs later.

## 4. When to stop

Use the agent loop when the review needs to gather context, but stop once the
review itself is stable. For code review, that usually means:

- inspect a small, explicit file set
- keep the system prompt short
- request concrete fixes, not a long essay
- record metrics so you can compare review quality over time

If you need a richer workflow, combine this with the eval tutorial and the
debugging tools in `docs/src/debugging.md`.
