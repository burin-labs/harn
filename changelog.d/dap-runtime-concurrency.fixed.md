- **Harn DAP now drives debuggee execution on a multi-threaded Tokio runtime with
  a persistent local task set (#2691).** Debug sessions can run `parallel`
  blocks and lifecycle pool workers without diverging from the VM concurrency
  paths used by normal execution.
- **DAP source requests now parse `file:` URIs with the standard URL parser.**
  This keeps source lookup correct for platform-native paths, including Windows
  drive-letter paths and `localhost` file URIs.
