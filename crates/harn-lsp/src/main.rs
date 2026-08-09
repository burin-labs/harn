//! Thin binary shim for the Harn language server.
//!
//! All logic lives in the `harn_lsp` library so the single multi-call `harn`
//! binary can dispatch into it (see `harn-cli`). This binary stays buildable
//! for local development and as a standalone artifact.

fn main() {
    // The language server compiles and type-checks Harn programs, so it needs
    // the same stack the CLI gives the VM. `harn lsp` already gets that from
    // the multi-call binary's runtime thread; this standalone entry point would
    // otherwise run on the process main thread — 8 MiB on Unix, 1 MiB on
    // Windows.
    std::thread::Builder::new()
        .name("harn-lsp".to_string())
        .stack_size(harn_vm::RUNTIME_STACK_SIZE)
        .spawn(harn_lsp::run)
        .expect("spawn harn-lsp runtime thread")
        .join()
        .expect("harn-lsp runtime thread panicked");
}
