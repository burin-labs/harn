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
///
/// The size is set by the deepest descent the runtime promises to *refuse*
/// rather than the deepest it expects to run. A nested agent descent costs
/// roughly 2 MiB of native stack per level, so the previous 16 MiB could carry
/// only seven levels while the nested-execution budget declares eight: the
/// refusal was undeliverable, and the process aborted on the level that should
/// have been denied. A bound the stack cannot reach is not a bound.
pub const RUNTIME_STACK_SIZE: usize = 32 * 1024 * 1024;

/// Run `body` on a thread that holds the [`RUNTIME_STACK_SIZE`] contract.
///
/// A caller that drives the VM from a thread it did not create borrows
/// whatever stack that thread was given. The test harness is where this keeps
/// happening: a case that builds a Tokio runtime on the libtest thread creates
/// no thread of its own, so it runs the VM on libtest's stack. That stack is
/// large enough only because every CI lane exports `RUST_MIN_STACK`, and a
/// developer machine without it aborts the whole test binary on one ordinary
/// agent loop (harn#7962). An abort is not a failed case: every later case in
/// the binary silently never runs.
///
/// Naming the thread the contract binds is the fix. A host that already spawns
/// its own VM thread with [`RUNTIME_STACK_SIZE`] does not need this; it exists
/// so a caller running on a borrowed stack can state the size once, at the
/// entry point, instead of depending on an environment variable no shipped
/// binary sets.
///
/// Panics propagate to the caller unchanged, so a failing assertion inside
/// `body` still fails its own test.
pub fn on_vm_stack<R: Send>(body: impl FnOnce() -> R + Send) -> R {
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name("harn-vm-contract-stack".to_owned())
            .stack_size(RUNTIME_STACK_SIZE)
            .spawn_scoped(scope, body)
            .expect("spawn a thread holding the VM stack contract")
            .join()
            .unwrap_or_else(|payload| std::panic::resume_unwind(payload))
    })
}

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
    #[expect(
        clippy::string_slice,
        reason = "len is a sum of whole split_inclusive line lengths"
    )]
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
                        #[expect(
                            clippy::string_slice,
                            reason = "offset is a match_indices offset on source"
                        )]
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

    /// A multi-thread Tokio runtime spawns its own worker threads, and those
    /// workers run the VM.
    ///
    /// [`vm_driving_threads_ask_for_the_runtime_stack`] cannot see this: it
    /// judges *spawn sites*, and a multi-thread runtime built on the process's
    /// main thread has none. Tokio creates the workers internally, at its own
    /// 2 MiB default, and the CLI's runtime went that way for five releases
    /// (harn#7961) — stopping a background sub-agent is polled on a worker, and
    /// at the default size that overflowed and aborted the process. The
    /// dedicated VM thread's `RUNTIME_STACK_SIZE` says nothing about a thread
    /// Tokio made.
    ///
    /// So every shipped multi-thread runtime must state
    /// [`super::RUNTIME_STACK_SIZE`] for its workers. Unlike a behavioral test,
    /// this fires under any ambient stack, which matters because the Rust test
    /// lanes export `RUST_MIN_STACK=16777216` and would otherwise hide the
    /// whole class.
    ///
    /// Test code is out of scope: a path component named `tests` or ending in
    /// `_tests`, and anything after a file's first `#[cfg(test)]`, is skipped.
    /// Those runtimes never ship, and they run under the lanes' large stack.
    #[test]
    fn multi_thread_runtimes_size_their_worker_threads() {
        /// Tokio builds these workers itself; nothing at the call site spawns.
        const MULTI_THREAD: &str = "Builder::new_multi_thread()";
        /// The builder method that states a worker stack size.
        const SIZES_WORKERS: &str = "thread_stack_size(";

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
                    && !entry.path().components().any(|component| {
                        let part = component.as_os_str().to_string_lossy();
                        let stem = part.strip_suffix(".rs").unwrap_or(&part);
                        stem == "tests" || stem.ends_with("_tests")
                    })
            })
        {
            scanned += 1;
            let source = std::fs::read_to_string(entry.path()).expect("read Rust source");
            let test_mod = source.find("\n#[cfg(test)]").unwrap_or(source.len());
            for (offset, _) in source.match_indices(MULTI_THREAD) {
                if offset > test_mod {
                    continue;
                }
                let end = (offset + WINDOW).min(source.len());
                let Some(window) = source.get(offset..end) else {
                    continue;
                };
                // The builder expression ends at its `build()`; past that is
                // unrelated code that could carry the marker by accident.
                #[expect(
                    clippy::string_slice,
                    reason = "find returns a char boundary in window"
                )]
                let window = match window.find(".build()") {
                    Some(cut) => &window[..cut],
                    None => window,
                };
                if !window.contains(SIZES_WORKERS) {
                    #[expect(
                        clippy::string_slice,
                        reason = "offset is a match_indices offset on source"
                    )]
                    let line = 1 + source[..offset].matches('\n').count();
                    offenders.push(format!("{}:{line}", entry.path().display()));
                }
            }
        }

        assert!(scanned > 100, "scan found only {scanned} sources to check");
        assert!(
            offenders.is_empty(),
            "these multi-thread Tokio runtimes ship, so their worker threads run \
             the VM, but they leave Tokio's 2 MiB default in place and a deep \
             enough frame aborts the process. Add \
             `.thread_stack_size(harn_vm::RUNTIME_STACK_SIZE)`:\n  {}",
            offenders.join("\n  ")
        );
    }

    /// Building a Tokio runtime is how a test says "this thread is about to
    /// drive the VM"; executing a compiled chunk is it doing so.
    const TEST_BUILDS_RUNTIME: &str = "tokio::runtime::Builder::new_";
    /// The VM entry point these tests reach.
    const TEST_DRIVES_VM: &str = "execute(&chunk";
    /// Entering through the contract helper is what makes the case stack-size
    /// independent, and it is the only accepted answer here: an inline
    /// `stack_size` on a thread the test spawns itself is the shape the first
    /// scan already judges.
    const ENTERS_CONTRACT: &str = "on_vm_stack(";

    /// Integration-test files that still drive the VM on whatever stack the
    /// libtest harness handed them.
    ///
    /// This is a shrinking ratchet, not a permission list. A row here is a
    /// file whose cases pass today only because every CI lane exports
    /// `RUST_MIN_STACK`; each one is a `super::on_vm_stack` wrap away from
    /// leaving. Rows may be removed, never added, and a row that is no longer
    /// an offender must be removed — a stale allowance is how a ratchet stops
    /// measuring anything.
    const HARNESS_STACK_BASELINE: [&str; 27] = [
        "harn-vm/tests/agent_inbox_e2e.rs",
        "harn-vm/tests/agent_terminal_ledger.rs",
        "harn-vm/tests/call_frame_allocations.rs",
        "harn-vm/tests/command_ledger_hold_paused_clock.rs",
        "harn-vm/tests/harn_vm/agent_fanout.rs",
        "harn-vm/tests/harn_vm/agent_loop_output_schema.rs",
        "harn-vm/tests/harn_vm/agent_loop_steering_seams.rs",
        "harn-vm/tests/harn_vm/agent_mcp_mid_conversation.rs",
        "harn-vm/tests/harn_vm/agent_mcp_tool_ceiling.rs",
        "harn-vm/tests/harn_vm/agent_prompt_prefix_stability.rs",
        "harn-vm/tests/harn_vm/agent_sessions.rs",
        "harn-vm/tests/harn_vm/builtin_call_dispatch.rs",
        "harn-vm/tests/harn_vm/compaction_policy_primitive.rs",
        "harn-vm/tests/harn_vm/external_agent_errors.rs",
        "harn-vm/tests/harn_vm/github_stdlib_connectors.rs",
        "harn-vm/tests/harn_vm/host_tool_batch_overlap.rs",
        "harn-vm/tests/harn_vm/pool_multithread.rs",
        "harn-vm/tests/harn_vm/runtime_introspection.rs",
        "harn-vm/tests/harn_vm/skill_activation_evidence_conformance.rs",
        "harn-vm/tests/harn_vm/stdlib_event_registration.rs",
        "harn-vm/tests/harn_vm/tool_call_cancellation.rs",
        "harn-vm/tests/harn_vm/tool_calling_bootcamp.rs",
        "harn-vm/tests/harn_vm/tool_input_schema_spelling.rs",
        "harn-vm/tests/harn_vm/tool_ref.rs",
        "harn-vm/tests/harn_vm/worker_overlap.rs",
        "harn-vm/tests/portable_kernel_parity.rs",
        "harn-vm/tests/support/mod.rs",
    ];

    /// A test that drives the VM on the harness thread escapes both scans
    /// above, and the contract with it.
    ///
    /// [`vm_driving_threads_ask_for_the_runtime_stack`] judges spawn sites and
    /// [`multi_thread_runtimes_size_their_worker_threads`] judges the workers
    /// Tokio makes. A case that builds a current-thread runtime on the libtest
    /// thread has neither: it spawns nothing, and Tokio spawns nothing for it.
    /// It runs the VM on libtest's stack, which is large enough only because
    /// the lanes export `RUST_MIN_STACK=16777216`.
    ///
    /// That is not a test-only concern. It makes the suite's green a statement
    /// about the lane's environment rather than about the code, and when it
    /// does break it breaks as `SIGABRT`, which kills the whole binary so every
    /// later case silently never runs (harn#7962). Two cases in
    /// `agent_loop_final_wrapup` and both cases in `workflow_replay_byte_compat`
    /// abort this way once the ambient stack drops.
    #[test]
    fn vm_driving_tests_enter_through_the_contract_stack() {
        let crates_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("harn-vm lives below crates");
        let tests_dir = crates_dir.join("harn-vm").join("tests");

        let mut offenders = Vec::new();
        let mut scanned = 0usize;
        for entry in walkdir::WalkDir::new(&tests_dir)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry.file_type().is_file()
                    && entry.path().extension().and_then(std::ffi::OsStr::to_str) == Some("rs")
            })
        {
            scanned += 1;
            let source = std::fs::read_to_string(entry.path()).expect("read Rust source");
            if !source.contains(TEST_BUILDS_RUNTIME) || !source.contains(TEST_DRIVES_VM) {
                continue;
            }
            if source.contains(ENTERS_CONTRACT) {
                continue;
            }
            let relative = entry
                .path()
                .strip_prefix(crates_dir)
                .expect("scan stays under crates")
                .to_string_lossy()
                .replace('\\', "/");
            offenders.push(relative);
        }

        assert!(
            scanned > 20,
            "scan found only {scanned} test sources to check"
        );

        let unlisted: Vec<&String> = offenders
            .iter()
            .filter(|path| !HARNESS_STACK_BASELINE.contains(&path.as_str()))
            .collect();
        assert!(
            unlisted.is_empty(),
            "these tests build a Tokio runtime and execute a chunk on the libtest \
             harness thread, so they drive the VM on a stack nobody sized and abort \
             the whole test binary once the ambient stack drops. Wrap the entry \
             point in `harn_vm::on_vm_stack(|| {{ .. }})`:\n  {}",
            unlisted
                .iter()
                .map(|path| path.as_str())
                .collect::<Vec<_>>()
                .join("\n  ")
        );

        let stale: Vec<&str> = HARNESS_STACK_BASELINE
            .into_iter()
            .filter(|path| !offenders.iter().any(|found| found == path))
            .collect();
        assert!(
            stale.is_empty(),
            "these files are no longer offenders, so their rows in \
             HARNESS_STACK_BASELINE allow something that no longer exists. Remove \
             them; the ratchet only shrinks:\n  {}",
            stale.join("\n  ")
        );
    }
}
