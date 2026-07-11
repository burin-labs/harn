Added a `harn dap` subcommand that launches the Harn debug adapter (DAP) over
stdio, so the step-through debugger is reachable with just `harn` on your PATH
instead of only via the standalone `harn-dap` binary alias. It runs the same
adapter server. A new end-to-end smoke test drives the real binary over stdio —
initialize, set a breakpoint, launch, hit the breakpoint, read a live local
variable, and run to termination — so the debugger can't silently regress.
