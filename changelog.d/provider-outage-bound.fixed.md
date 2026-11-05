- A route's network breaker window now means the same time for every prompt in a
  long-lived process, because all real clocks share one monotonic origin. Before
  this, a window opened in one prompt was read through the next prompt's clock and
  looked longer by the time between the two prompts.
- An ACP prompt now lets its own first request probe a network-open route
  immediately. One probe runs at a time per route. If it gets no answer, the rest
  of that prompt's turn fails fast with `circuit_open` instead of waiting, and other
  sessions on the route wait out the window as usual. Embedders opt in by holding
  the guard from `harn_vm::llm::begin_interactive_prompt(session_id)` for the turn,
  inside its ambient scope.
- A breaker wait that sends nothing now reports `transient_network` and says no
  request was sent, not `timeout`.
- `with_retry` takes an optional `max_elapsed_ms` wall-clock budget.
