The native process-liveness probe and its Rust enum now live in `harn-vm-core`.
`harn-vm` and `harn-hostlib` re-export that owner instead of defining another probe.

Migration: rebuild Rust consumers against the matching Harn release. Existing
`harn_hostlib::process_liveness` imports remain available. Code that names the
defining crate for `ProcessLiveness` should use
`harn_vm_core::process_liveness::ProcessLiveness`.
