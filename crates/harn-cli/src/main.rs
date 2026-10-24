//! Entry point for the single multi-call `harn` binary; see [`harn_cli::multicall`].

/// Use mimalloc as the global allocator for the `harn` binary (and its
/// `harn-lsp` / `harn-dap` multi-call aliases, which share this entrypoint).
///
/// The Harn runtime is allocation-bound: copy-on-write `Arc` collections,
/// per-frame environment snapshots, transcript and event buffers, and LLM
/// payload marshaling all churn small allocations. mimalloc handles that
/// pattern with lower per-allocation latency and fragmentation than the
/// default system allocator. Behind the default-on `mimalloc` feature so a
/// `--no-default-features` build transparently falls back to the system
/// allocator.
#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() {
    harn_cli::multicall::main();
}
