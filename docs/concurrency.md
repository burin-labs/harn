# Concurrency

Harn has built-in concurrency primitives that don't require callbacks, promises, or async/await boilerplate.

## spawn and await

Launch background tasks and collect results:

```javascript
let handle = spawn {
  sleep(1s)
  "done"
}

let result = await(handle)  // blocks until complete
log(result)                 // "done"
```

Cancel a task before it finishes:

```javascript
let handle = spawn { sleep(10s) }
cancel(handle)
```

Each spawned task runs in an isolated interpreter instance.

## parallel

Run N tasks concurrently and collect results in order:

```javascript
let results = parallel(5) { i ->
  i * 10
}
// [0, 10, 20, 30, 40]
```

The variable `i` is the zero-based task index. Results are always returned
in index order regardless of completion order.

## parallel_map

Map over a collection concurrently:

```javascript
let files = ["a.txt", "b.txt", "c.txt"]

let contents = parallel_map(files) { file ->
  read_file(file)
}
```

Results preserve the original list order.

## retry

Automatically retry a block that might fail:

```javascript
retry 3 {
  http_get("https://flaky-api.example.com/data")
}
```

Executes the body up to N times. If the body succeeds, returns immediately.
If all attempts fail, returns `nil`. Note that `return` statements inside
`retry` propagate out (they are not retried).

## Channels

Message-passing between concurrent tasks:

```javascript
let ch = channel("events")
send(ch, {event: "start", timestamp: timestamp()})
let msg = receive(ch)
```

## Channel iteration

You can iterate over a channel with a `for` loop. The loop receives
messages one at a time and exits when the channel is closed and fully
drained:

```javascript
let ch = channel("stream")

spawn {
  send(ch, "chunk 1")
  send(ch, "chunk 2")
  close_channel(ch)
}

for chunk in ch {
  log(chunk)
}
// prints "chunk 1" then "chunk 2", then the loop ends
```

This is especially useful with `llm_stream`, which returns a channel
of response chunks:

```javascript
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

```javascript
let counter = atomic(0)
log(atomic_get(counter))         // 0

let c2 = atomic_add(counter, 5)
log(atomic_get(c2))              // 5

let c3 = atomic_set(c2, 100)
log(atomic_get(c3))              // 100
```

Atomic operations return new atomic values (they don't mutate in place).

## Mutex

Mutual exclusion for critical sections:

```javascript
mutex {
  // only one task executes this block at a time
  var count = count + 1
}
```

## Deadline

Set a timeout on a block of work:

```javascript
deadline 30s {
  // must complete within 30 seconds
  agent_loop(task, system, {persistent: true})
}
```
