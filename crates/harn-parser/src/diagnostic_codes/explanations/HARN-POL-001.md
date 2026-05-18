# HARN-POL-001 — pool backpressure rejected a submit

`pool.submit(...)` attempted to enqueue work into a bounded pool whose
`backpressure` policy was already full and configured with
`on_full: "fail_submitter"`.

Increase the queue depth, choose a different `on_full` policy, wait for
existing task handles to drain, or catch the error and retry from your
own orchestration policy.
