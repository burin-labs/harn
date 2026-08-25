# Tutorial: build a code review agent

This tutorial shows a small but realistic review pipeline. The goal is not to
rebuild a full IDE integration. Instead, we want a deterministic Harn program
that can review a patch, inspect context, and return a concise report.

Use the companion example as a starting point:

```bash
harn run examples/code-reviewer.harn -- "$(git diff)"
```

## 1. Start with a tight review prompt

The simplest useful reviewer is just an LLM call with a strong system prompt.
Keep the instructions short, specific, and opinionated:

```harn
pipeline default(harness: Harness, task) {
  const system = """
You are a senior code reviewer.
Review the patch for correctness, security, maintainability, and tests.
Return:
- must-fix issues
- suggestions
- missing tests
End with a short verdict.
"""

  const review = harness.llm.call(task, system, {
    temperature: 0.2,
    max_tokens: 1200,
  })

  harness.stdio.log(review.text)
}
```

This is enough when the user pastes a diff directly into `task`.

The `task` parameter is supplied by whatever drives the pipeline — an editor
host over ACP, `harn serve`, or a caller that runs the pipeline for you.
`harn run` does not bind it, so a pipeline you want to run straight from the
CLI reads its input from `argv` instead, the way the companion example above
does. Pick the shape that matches how the reviewer will be invoked.

## 2. Add file context when you need it

Real review agents usually need a bit of surrounding code. The simplest route
is to read a small, explicit list of files and combine them with the patch.
Keep the list short so the prompt stays focused.

```harn
pipeline default(harness: Harness, task) {
  const files = ["src/main.rs", "src/lib.rs"]
  let context = ""

  for file in files {
    context = context + "\n\n=== " + file + " ===\n"
      + harness.fs.read_text(file)
  }

  const review = harness.llm.call(
    "Patch:\n" + task + "\n\nContext:\n" + context,
    """
You are a strict code reviewer.
Flag correctness bugs first, then test gaps, then maintainability issues.
Do not invent missing context. If the context is insufficient, say so.
""",
    {temperature: 0.2, max_tokens: 1400}
  )

  harness.stdio.log(review.text)
}
```

If you want to review a directory tree instead, use `harness.fs.list_dir()` and
`parallel each` to gather files concurrently, then trim the result to the most
relevant ones before calling the model.

## 3. Make the review measurable

Good review agents should record something observable, even if it is only a
small heuristic. Use `eval_metric()` to track whether the agent found issues
and how often it asked for more context.

```harn
pipeline default(harness: Harness, task) {
  const review = harness.llm.call(
    task,
    "You are a code reviewer. Return a concise bullet list.",
    {temperature: 0.2}
  )

  const has_issue = review.text.contains("issue")
    || review.text.contains("bug")
  eval_metric("review_has_issue", has_issue)
  eval_metric("review_chars", review.text.count)

  harness.stdio.log(review.text)
}
```

Recorded metrics are printed by the run and, for a workflow run, saved into the
run record under `.harn-runs/` that `harn eval` reads. See
[the eval pipeline tutorial](./tutorial-eval-pipeline.md) for that path.

## 4. When to stop

Use the agent loop when the review needs to gather context, but stop once the
review itself is stable. For code review, that usually means:

- inspect a small, explicit file set
- keep the system prompt short
- request concrete fixes, not a long essay
- record metrics so you can compare review quality over time

If you need a richer workflow, combine this with the eval tutorial and the
[debugging tools](./debugging.md).
