# Concurrency

Harn has built-in concurrency primitives that don't require callbacks, promises, or async/await boilerplate.

## spawn and await

Launch background tasks and collect results:

```harn
let handle = spawn {
  sleep(1s)
  "done"
}

let result = await(handle)  // blocks until complete
println(result)                 // "done"
```

Cancel a task before it finishes:

```harn
let handle = spawn { sleep(10s) }
cancel(handle)
```

Each spawned task runs in an isolated interpreter instance.

## parallel

Run N tasks concurrently and collect results in order:

```harn
let results = parallel(5) { i ->
  i * 10
}
// [0, 10, 20, 30, 40]
```

The variable `i` is the zero-based task index. Results are always returned
in index order regardless of completion order.

## parallel each

Map over a collection concurrently:

```harn
let files = ["a.txt", "b.txt", "c.txt"]

let contents = parallel each files { file ->
  read_file(file)
}
```

Results preserve the original list order.

## parallel settle

Like `parallel each`, but never throws. Instead, it collects both
successes and failures into a result object:

```harn
let items = [1, 2, 3]
let outcome = parallel settle items { item ->
  if item == 2 {
    throw "boom"
  }
  item * 10
}

println(outcome.succeeded)  // 2
println(outcome.failed)     // 1

for r in outcome.results {
  if is_ok(r) {
    println(unwrap(r))
  } else {
    println(unwrap_err(r))
  }
}
```

The return value is a dict with:

| Field | Type | Description |
|---|---|---|
| `results` | list | List of `Result` values (one per item), in order |
| `succeeded` | int | Number of `Ok` results |
| `failed` | int | Number of `Err` results |

This is useful when you want to process all items and handle failures
after the fact, rather than aborting on the first error.

## retry

Automatically retry a block that might fail:

```harn
retry 3 {
  http_get("https://flaky-api.example.com/data")
}
```

Executes the body up to N times. If the body succeeds, returns immediately.
If all attempts fail, returns `nil`. Note that `return` statements inside
`retry` propagate out (they are not retried).

## Channels

Message-passing between concurrent tasks:

```harn
let ch = channel("events")
send(ch, {event: "start", timestamp: timestamp()})
let msg = receive(ch)
```

## Channel iteration

You can iterate over a channel with a `for` loop. The loop receives
messages one at a time and exits when the channel is closed and fully
drained:

```harn
let ch = channel("stream")

spawn {
  send(ch, "chunk 1")
  send(ch, "chunk 2")
  close_channel(ch)
}

for chunk in ch {
  println(chunk)
}
// prints "chunk 1" then "chunk 2", then the loop ends
```

This is especially useful with `llm_stream`, which returns a channel
of response chunks:

```harn
let stream = llm_stream("Tell me a story", "You are a storyteller")
for chunk in stream {
  print(chunk)
}
```

Use `try_receive(ch)` for non-blocking reads -- it returns `nil`
immediately if no message is available. Use `close_channel(ch)` to
signal that no more messages will be sent.

## Atomics

Thread-safe counters:

```harn
let counter = atomic(0)
println(atomic_get(counter))         // 0

let c2 = atomic_add(counter, 5)
println(atomic_get(c2))              // 5

let c3 = atomic_set(c2, 100)
println(atomic_get(c3))              // 100
```

Atomic operations return new atomic values (they don't mutate in place).

## Mutex

Mutual exclusion for critical sections:

```harn
mutex {
  // only one task executes this block at a time
  var count = count + 1
}
```

## Deadline

Set a timeout on a block of work:

```harn
deadline 30s {
  // must complete within 30 seconds
  agent_loop(task, system, {persistent: true})
}
```

## Defer

Register cleanup code that runs when the enclosing scope exits, whether
by normal return or by a thrown error:

```harn
let f = open("data.txt")
defer { close(f) }
// ... use f ...
// close(f) runs automatically on scope exit
```

Multiple `defer` blocks execute in LIFO (last-registered, first-executed)
order, similar to Go's `defer`.

## Capping in-flight work with `max_concurrent`

`parallel each`, `parallel settle`, and `parallel N` all accept an
optional `with { max_concurrent: N }` clause that caps how many
workers are in flight at once. Tasks past the cap wait until a slot
frees up — fan-out stays bounded while the total work is unchanged.

```harn
// Without a cap: all 200 requests hit the server at once.
let results = parallel settle paths { p -> llm_call(p, nil, opts) }

// With max_concurrent=8: at most 8 in-flight calls at any moment.
let results = parallel settle paths with { max_concurrent: 8 } { p ->
  llm_call(p, nil, opts)
}
```

`max_concurrent: 0` (or a missing `with` clause) means unlimited.
Negative values are treated as unlimited. The cap applies to every
parallel mode, including the count form:

```harn
parallel 100 with { max_concurrent: 4 } { i ->
  process(i)
}
```

## Rate limiting LLM providers

`max_concurrent` bounds *simultaneous* in-flight tasks on the caller's
side. A provider can additionally be rate-limited at the throughput
layer (requests per minute). The RPM limiter is a sliding-window
budget enforced before each `llm_call` / `llm_completion` — requests
past the budget wait for the window to free up rather than error.

Configure RPM per provider via:

- `rpm: 600` in the provider's entry in `providers.toml` / `harn.toml`.
- `HARN_RATE_LIMIT_<PROVIDER>=600` environment variable
  (e.g. `HARN_RATE_LIMIT_TOGETHER=600`, `HARN_RATE_LIMIT_LOCAL=60`).
  Env overrides config.
- `llm_rate_limit("provider", 600)` at runtime from a pipeline.

The two controls compose: `max_concurrent` prevents bursts from
saturating the server; RPM shapes sustained throughput. When batching
hundreds of LLM calls against a local single-GPU server, both are
worth setting — otherwise the RPM budget can be spent in a 2-second
burst that overwhelms the queue and drops requests.
