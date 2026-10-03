- `harn serve` on Windows no longer overflows the 1 MiB main-thread stack
  before it answers: the CLI recognizes serve transports on its sized runtime
  thread, and `harn.exe` links an 8 MiB main-thread stack to match Unix.
