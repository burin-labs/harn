//! Thin binary shim for the Harn debug adapter.
//!
//! All logic lives in the `harn_dap` library so the single multi-call `harn`
//! binary can dispatch into it (see `harn-cli`). This binary stays buildable
//! for local development and as a standalone artifact.

fn main() {
    // The debug adapter drives the VM, so it needs the same stack the CLI
    // gives it. The multi-call `harn dap` alias sizes its own adapter thread
    // (see `harn_cli::entrypoint::run_dap_adapter`, which spawns a fresh
    // thread to escape the CLI's Tokio runtime and therefore inherits nothing
    // from it); this standalone entry point would otherwise run the VM on the
    // process main thread — 8 MiB on Unix, 1 MiB on Windows.
    std::thread::Builder::new()
        .name("harn-dap".to_string())
        .stack_size(harn_vm::RUNTIME_STACK_SIZE)
        .spawn(harn_dap::run)
        .expect("spawn harn-dap runtime thread")
        .join()
        .expect("harn-dap runtime thread panicked");
}
