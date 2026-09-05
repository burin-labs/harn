- **The supervisor command dispatcher no longer carries every subcommand's
  state in one stack frame (#7931).** `harn orchestrator supervisor` matched on
  its subcommand and awaited each `async fn` inline, so the dispatch frame held
  all eleven futures' states at once and measured within five percent of the
  stack size that aborts a tokio worker. Each arm is boxed before it is
  awaited, so the frame holds a pointer and no longer grows with the number of
  subcommands.
