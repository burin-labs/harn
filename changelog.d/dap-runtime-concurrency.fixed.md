- **Harn DAP now drives debuggee execution on a multi-threaded Tokio runtime with
  a persistent local task set (#2691).** Debug sessions can run `parallel`
  blocks and lifecycle pool workers without diverging from the VM concurrency
  paths used by normal execution.
