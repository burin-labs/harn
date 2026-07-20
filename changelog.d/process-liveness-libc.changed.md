- **Process-liveness probes use maintained `libc` bindings.** The host-lease liveness probe and the VM's
  process-cleanup wait no longer hand-declare `extern "C" { fn kill }`, and the macOS process-generation
  identity no longer carries a hand-transcribed 22-field `repr(C)` copy of `proc_bsdinfo`. Both now use the
  `libc` crate's bindings. Behavior is unchanged on every platform — the `ESRCH`-only definition of "dead"
  and the microsecond-resolution start-time identity that defends against PID reuse are both preserved
  exactly — but the Apple struct layout is now maintained upstream instead of being a local transcription
  that could silently drift from the system header and yield a wrong lease-owner identity.
