- **Sandboxed builds get a writable, workspace-local `TMPDIR`.** Compiler
  linkers (`rustc`/`cc`/`ld`, Go, Swift, …) and other toolchains write
  intermediate object/temp files to `$TMPDIR`, defaulting to the system `/tmp`
  when it is unset — which is outside the sandbox's writable workspace roots, so
  those writes were denied and a build that should pass FALSE-FAILED with
  `could not write output to /tmp/rustcXXXX/…: Cannot create temporary file in
  /tmp/: Permission denied`. The process command-runner (both the
  `host_call("process", …)` exec/spawn path and the `process.exec`/`shell`
  builtins) now points a sandboxed child's `TMPDIR`/`TMP`/`TEMP` at a lazily
  created `.harn-tmp/` inside the first writable workspace root, which the OS
  sandbox already grants. This fixes any TMPDIR-honoring toolchain without
  widening the sandbox; a `TMPDIR` the caller sets explicitly is respected, and
  the temp dir self-`.gitignore`s so its churn never leaks into a diff or eval
  grading.
- **Linux sandbox no longer denies `socketpair` below the network ceiling.**
  The seccomp blocklist conflated the anonymous, unaddressable local-IPC
  `socketpair` with egress sockets, so `cargo build`/`cargo test` could not even
  spawn `rustc` (Cargo's jobserver is `socketpair`-backed) — it failed with
  `(never executed)` / `Operation not permitted`. `socketpair` is now allowed
  while `socket`/`connect`/`bind`/`listen`/`send*`/`recv*` stay denied, so local
  IPC works without opening any egress path.
