//! The native stack contract for threads that drive the Harn VM.
//!
//! Harn has two independent stack hazards, each with its own owner:
//!
//! * Walking an arbitrarily deep *value* — `x = [x]` in a loop — is made
//!   stack-size independent by [`crate::value::recursion`], which grows the
//!   native stack on demand and tears values down iteratively.
//! * Walking an arbitrarily deep *program* — parse, type-check, compile, and
//!   evaluate all recurse over nested syntax — is not. It relies on the thread
//!   simply having enough stack, and that is what [`RUNTIME_STACK_SIZE`] is.
//!
//! The second contract lives entirely in the hosts: it holds only if every
//! thread that ends up running the VM asks for the size. Getting it wrong is
//! unusually expensive, because a stack overflow aborts the process instead of
//! failing one request, so this module also carries the structural check that
//! keeps new hosts honest.

/// Native stack size a thread needs in order to drive the Harn VM.
///
/// Compilation and execution walk nested program structure with recursive
/// frames, which can exceed Rust's 2 MiB default thread stack. A host that
/// runs the VM on a thread it spawns must request this size explicitly.
///
/// Relying on the ambient default is not safe, and neither is relying on
/// `RUST_MIN_STACK`: that variable is set by the CI test lanes but not by any
/// shipped binary, so a host that depends on it passes its own tests and then
/// aborts the whole process — a stack overflow is not a catchable panic — the
/// first time a customer runs a deep enough script.
pub const RUNTIME_STACK_SIZE: usize = 16 * 1024 * 1024;

#[cfg(test)]
mod tests {
    /// How much source to read past a spawn before deciding what it does.
    const WINDOW: usize = 600;

    /// Spawning forms that create a thread with the ambient default stack
    /// unless the call site says otherwise.
    const SPAWNS: [&str; 2] = ["std::thread::spawn(", "thread::Builder::new()"];

    /// Building a current-thread Tokio runtime on a freshly spawned thread is
    /// what "this thread is about to drive the VM" looks like across the
    /// workspace: it is how every serve transport, the orchestrator's ACP
    /// worker, and the CLI's scaffold and test workers are shaped.
    const DRIVES_VM: &str = "tokio::runtime::Builder";

    /// Either idiom for honoring the contract: the inline
    /// `.stack_size(..._STACK_SIZE)` that `harn-cli` uses, or a helper such as
    /// `harn-serve`'s `vm_thread` that applies it centrally (those call sites
    /// match neither spawn form, so they never reach this check).
    const HONORS_CONTRACT: &str = "stack_size(";

    /// The source a spawn is judged on: everything from the spawn up to the
    /// next attributed item, at any indentation.
    ///
    /// Without the cut, a one-line spawn reads the runtime built by the
    /// *following* function and reports a thread that does no VM work.
    fn spawn_body(window: &str) -> &str {
        match window
            .split_inclusive('\n')
            .take_while(|line| !line.trim_start().starts_with("#["))
            .map(str::len)
            .sum::<usize>()
        {
            0 => "",
            len => &window[..len],
        }
    }

    /// Every thread in the workspace that builds a Tokio runtime to drive the
    /// VM must ask for [`super::RUNTIME_STACK_SIZE`].
    ///
    /// This is deliberately a *workspace* scan rather than a per-crate one.
    /// The same defect shipped simultaneously in `harn-serve`, `harn-cli`, and
    /// `harn-vm` (harn#6165) precisely because each crate's own tests could
    /// only see that crate — and because every Rust test lane here exports
    /// `RUST_MIN_STACK=16777216`, which makes an unsized spawn large enough in
    /// CI and nowhere else.
    ///
    /// Scope is `crates/`, which is every workspace member and so every shipped
    /// host. `bench/` is out, and builds its runtimes on the current thread
    /// rather than a spawned one, so there is nothing there to catch.
    ///
    /// What this does *not* catch, so nobody over-trusts it: a thread that
    /// drives the VM without building a Tokio runtime on itself. Of the ~76
    /// non-test spawn sites in the workspace this judges only the ~10 shaped
    /// like a transport or worker. `run_dap_adapter`, the counterfactual plan
    /// runner, and the connector worker loop all drive the VM through a plain
    /// function call and were found by reading, not by this scan. Deciding
    /// those needs a call graph; recognizing the idiom that actually recurs
    /// does not, and that idiom is where every instance so far has lived.
    #[test]
    fn vm_driving_threads_ask_for_the_runtime_stack() {
        let crates_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("harn-vm lives below crates");

        let mut offenders = Vec::new();
        let mut scanned = 0usize;
        for entry in walkdir::WalkDir::new(crates_dir)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry.file_type().is_file()
                    && entry.path().extension().and_then(std::ffi::OsStr::to_str) == Some("rs")
                    && entry
                        .path()
                        .components()
                        .any(|component| component.as_os_str() == "src")
                    && entry.file_name() != "runtime_stack.rs"
            })
        {
            scanned += 1;
            let source = std::fs::read_to_string(entry.path()).expect("read Rust source");
            for pattern in SPAWNS {
                for (offset, _) in source.match_indices(pattern) {
                    let end = (offset + WINDOW).min(source.len());
                    // A window can land mid-codepoint; the next match still
                    // covers this file and every marker here is ASCII.
                    let Some(window) = source.get(offset..end) else {
                        continue;
                    };
                    let window = spawn_body(window);
                    if window.contains(DRIVES_VM) && !window.contains(HONORS_CONTRACT) {
                        let line = 1 + source[..offset].matches('\n').count();
                        offenders.push(format!("{}:{line}", entry.path().display()));
                    }
                }
            }
        }

        assert!(scanned > 100, "scan found only {scanned} sources to check");
        assert!(
            offenders.is_empty(),
            "these threads build a Tokio runtime to drive the VM but take Rust's \
             2 MiB default stack, so a deep script aborts the process instead of \
             failing one request. Give them harn_vm::RUNTIME_STACK_SIZE — inline \
             via `.stack_size(..)`, or through a helper like harn-serve's \
             `vm_thread`:\n  {}",
            offenders.join("\n  ")
        );
    }
}
