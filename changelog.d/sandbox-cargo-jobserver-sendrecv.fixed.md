- **Linux sandbox no longer denies the `send*`/`recv*` family below the network
  ceiling, so Cargo's socketpair-backed jobserver works.** Un-denying
  `socketpair` (so the jobserver's `SOCK_SEQPACKET` pair can be created) was
  necessary but not sufficient: Cargo acquires/releases build tokens over that
  pair with `recvfrom`/`sendto`, and those stayed seccomp-denied — so the
  parent's token read returned `EPERM`, surfacing as a worker-thread
  `the CLOEXEC pipe failed: Operation not permitted` panic that aborted
  `cargo build`/`cargo test` before any `rustc`/link step. `recvfrom`,
  `recvmsg`, `sendmsg`, and `sendto` are now allowed below the network ceiling.
  They open no egress: with `socket`/`connect`/`bind`/`listen`/`accept` still
  denied, a sandboxed process can hold only anonymous `socketpair` pairs and
  pipes, so the send/recv family can only drive local IPC. Reproduced and
  verified under the exact seccomp filter — a trivial `cargo build` panics with
  the family denied and completes with it allowed, while `socket(AF_INET)` and
  `connect` stay `EPERM`.
