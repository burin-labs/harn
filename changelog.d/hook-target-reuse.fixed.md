Reduced local hook rebuild churn by giving hooks a deterministic per-worktree
Cargo target directory when none is configured and by running prompt-prose
checks through one resolved `harn` binary instead of repeated `cargo run`
invocations.
