# Tutorial: Build an eval pipeline

This tutorial builds a small evaluation loop that runs a set of examples,
records metrics, and produces an auditable summary. The goal is to make quality
visible, not to build an elaborate benchmark harness.

Use the companion example as a baseline:

```bash
cargo run --bin harn -- run examples/data-pipeline.harn
```

## 1. Define the dataset inline

Start with a tiny set of representative inputs. Keep the examples small enough
that you can inspect failures by eye:

```harn
pipeline main(task) {
  let cases = [
    {id: "case-1", input: "What is 2 + 2?", expected: "4"},
    {id: "case-2", input: "Capital of France?", expected: "Paris"},
    {id: "case-3", input: "Color of grass?", expected: "green"},
  ]

  println("Loaded ${cases.count} eval cases")
}
```

## 2. Run the cases in parallel

If each case is independent, use `parallel each` so the slow parts overlap.

```harn
pipeline main(task) {
  let cases = [
    {id: "case-1", input: "What is 2 + 2?", expected: "4"},
    {id: "case-2", input: "Capital of France?", expected: "Paris"},
    {id: "case-3", input: "Color of grass?", expected: "green"},
  ]

  let results = parallel each cases { tc ->
    let answer = llm_call(tc.input, "Answer in one word or short phrase.", {
      temperature: 0.0,
      max_tokens: 64,
    })

    {
      id: tc.id,
      expected: tc.expected,
      actual: answer.text,
      correct: answer.text.contains(tc.expected),
    }
  }

  println(json_stringify(results))
}
```

For a real eval suite, replace the inline `cases` list with a manifest or a
dataset file that your pipeline reads with `read_file()`.

## 3. Record metrics

The important part of an eval pipeline is the metric trail. Use
`eval_metric()` to record per-case and aggregate results.

```harn
pipeline main(task) {
  let cases = [
    {id: "case-1", input: "What is 2 + 2?", expected: "4"},
    {id: "case-2", input: "Capital of France?", expected: "Paris"},
  ]

  var passed = 0
  for tc in cases {
    let answer = llm_call(tc.input, "Answer in one word.", {temperature: 0.0})
    let correct = answer.text.contains(tc.expected)
    if correct {
      passed = passed + 1
    }
    eval_metric("case_correct", correct, {case_id: tc.id})
  }

  let accuracy = passed / cases.count
  eval_metric("accuracy", accuracy, {passed: passed, total: cases.count})
  eval_metric("run_id", uuid())
  eval_metric("generated_at", timestamp())
}
```

## 4. Export a report

Once the metrics are recorded, write a compact report so a later run can diff
the results.

```harn
pipeline main(task) {
  let summary = {
    run_id: uuid(),
    generated_at: timestamp(),
    accuracy: 0.83,
    notes: "Replace the fixed accuracy with real case scoring",
  }

  write_file("eval-summary.json", json_stringify(summary))
  println(json_stringify(summary))
}
```

## 5. How to use it

Run the pipeline, inspect the metrics, then compare runs over time:

```bash
harn run examples/eval-workflow.harn
harn eval .harn-runs/<run-id>.json
```

A good eval pipeline answers three questions:

- did the model improve?
- did latency or token usage regress?
- which cases failed, and why?
