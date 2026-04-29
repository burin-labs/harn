# Streams

Streams are lazy, single-pass values produced by `gen fn`. A stream
emits values over time and can be consumed with `for`, `.next()`, or
`.iter()`.

```harn
gen fn numbers(start: int, end: int) -> Stream<int> {
  var n = start
  while n < end {
    emit n
    n = n + 1
  }
}

for n in numbers(1, 4) {
  println(n)
}
```

`gen` is contextual in the `gen fn` declaration form, so existing
identifiers named `gen` remain valid. `emit expr` is only valid inside
`gen fn`. It sends one value to the consumer and then the function
continues when the consumer asks for the next item. Existing `yield`
behavior is unchanged; use `emit` for streams.

`Stream<T>` is distinct from `Generator<T>` in the checker. Regular
functions that already use `yield` keep returning `Generator<T>`.
`gen fn` returns `Stream<T>`.

```harn
gen fn chunks() -> Stream<string> {
  emit "one"
  emit "two"
}

let s: Stream<string> = chunks()
let first = s.next()
println(first.value)  // one
println(first.done)   // false
```

Errors thrown inside a stream propagate to the consumer at the point
where the next value is pulled:

```harn
gen fn broken() -> Stream<int> {
  emit 1
  throw "failed"
}

try {
  for n in broken() {
    println(n)
  }
} catch err {
  println("caught ${err}")
}
```

Breaking out of a `for` loop stops consuming the stream. Stream
operators such as map/filter/merge/throttle and built-in LLM token
streaming are separate runtime features layered on top of this base
value type.
