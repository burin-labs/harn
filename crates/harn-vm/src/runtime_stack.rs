//! The native stack contract for threads that drive the Harn VM.
//!
//! The contract and the only sanctioned ways to create such a thread live in
//! [`harn_parser::runtime_stack`], the lowest crate that recurses over a
//! program. This module re-exports them under the names hosts already use and
//! carries the workspace scans that keep every crate on them.

pub use harn_parser::runtime_stack::{
    builder, on_vm_stack, scope, spawn, RuntimeScope, RUNTIME_STACK_SIZE,
};

#[cfg(test)]
mod tests {
    /// How much source to read past a runtime builder before deciding.
    const WINDOW: usize = 600;

    /// The file allowed to create a thread directly, and this scanner, whose
    /// negative-control fixtures spell out every bypass.
    const EXEMPT: [&str; 2] = [
        "harn-parser/src/runtime_stack.rs",
        "harn-vm/src/runtime_stack.rs",
    ];

    /// Ways to create a thread that bypass the owner: `std::thread::spawn`,
    /// `std::thread::scope` (whose `Scope::spawn` always takes the default
    /// stack), and `std::thread::Builder`, imported or called by path; plus a
    /// `stack_size` call anywhere else, which would give the size a second
    /// author.
    fn bare_thread_creations(source: &str) -> Vec<usize> {
        let by_path = regex::Regex::new(r"\bthread::(spawn|scope|Builder)\b").expect("regex");
        let by_import =
            regex::Regex::new(r"\bthread::\{[^}]*\b(spawn|scope|Builder)\b").expect("regex");

        let mut lines = Vec::new();
        let mut in_import = String::new();
        for (index, line) in source.lines().enumerate() {
            let code = line.split("//").next().unwrap_or_default();
            // A brace import can span lines; judge it once it closes.
            if !in_import.is_empty() || code.contains("thread::{") {
                in_import.push_str(code);
                if !code.contains('}') {
                    continue;
                }
                let import = std::mem::take(&mut in_import);
                if by_import.is_match(&import) {
                    lines.push(index + 1);
                }
                continue;
            }
            if by_path.is_match(code) || code.contains(".stack_size(") {
                lines.push(index + 1);
            }
        }
        lines
    }

    /// Workspace crates whose normal dependencies reach `harn-parser`: every
    /// one of them can parse or run Harn code on a thread it creates.
    fn crates_that_reach_the_parser(crates_dir: &std::path::Path) -> Vec<String> {
        let mut deps = std::collections::BTreeMap::<String, Vec<String>>::new();
        for entry in std::fs::read_dir(crates_dir).expect("read crates dir") {
            let dir = entry.expect("crate entry").path();
            let Ok(manifest) = std::fs::read_to_string(dir.join("Cargo.toml")) else {
                continue;
            };
            let manifest: toml::Table = toml::from_str(&manifest).expect("parse Cargo.toml");
            let name = dir
                .file_name()
                .expect("crate dir")
                .to_string_lossy()
                .into_owned();
            let normal = manifest
                .get("dependencies")
                .and_then(toml::Value::as_table)
                .map(|table| table.keys().cloned().collect())
                .unwrap_or_default();
            deps.insert(name, normal);
        }
        let mut reaching = std::collections::BTreeSet::from(["harn-parser".to_owned()]);
        loop {
            let before = reaching.len();
            for (name, normal) in &deps {
                if normal.iter().any(|dep| reaching.contains(dep)) {
                    reaching.insert(name.clone());
                }
            }
            if reaching.len() == before {
                break;
            }
        }
        reaching.into_iter().collect()
    }

    /// Every thread a parser-reaching crate creates goes through
    /// `harn_parser::runtime_stack`.
    ///
    /// The scan used to judge only threads that built a Tokio runtime. A worker
    /// pool that only parses was invisible to it, and `harn_modules`'s parallel
    /// import loader shipped on 2 MiB stacks that overflowed on a dozen nesting
    /// levels (harn#9218). Deciding which threads reach the parser needs a call
    /// graph; refusing every other way to create a thread does not. Test code
    /// under `src/` is in scope too, because a test that runs Harn on a default
    /// stack passes only under the lanes' `RUST_MIN_STACK`.
    #[test]
    fn parser_reaching_crates_create_threads_only_through_the_owner() {
        let crates_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("harn-vm lives below crates");
        let reaching = crates_that_reach_the_parser(crates_dir);
        for expected in ["harn-modules", "harn-vm", "harn-cli", "harn-serve"] {
            assert!(
                reaching.iter().any(|name| name == expected),
                "{expected} reaches the parser but the dependency walk missed it: {reaching:?}"
            );
        }

        let mut offenders = Vec::new();
        let mut scanned = 0usize;
        for name in &reaching {
            for entry in walkdir::WalkDir::new(crates_dir.join(name).join("src"))
                .into_iter()
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry.file_type().is_file()
                        && entry.path().extension().and_then(std::ffi::OsStr::to_str) == Some("rs")
                })
            {
                let relative = entry
                    .path()
                    .strip_prefix(crates_dir)
                    .expect("scan stays under crates")
                    .to_string_lossy()
                    .replace('\\', "/");
                if EXEMPT.contains(&relative.as_str()) {
                    continue;
                }
                scanned += 1;
                let source = std::fs::read_to_string(entry.path()).expect("read Rust source");
                for line in bare_thread_creations(&source) {
                    offenders.push(format!("{relative}:{line}"));
                }
            }
        }

        assert!(scanned > 100, "scan found only {scanned} sources to check");
        assert!(
            offenders.is_empty(),
            "these sites create a thread in a crate that can parse or run Harn code \
             without `harn_parser::runtime_stack`, so it gets Rust's 2 MiB default \
             and a deep program aborts the process. Use `runtime_stack::spawn`, \
             `runtime_stack::builder()`, or `runtime_stack::scope`:\n  {}",
            offenders.join("\n  ")
        );
    }

    /// The negative control: the matcher flags every bypass and none of the
    /// sanctioned forms, so a clean scan means clean sources.
    #[test]
    fn bare_thread_matcher_flags_bypasses_and_spares_the_owner_api() {
        let flagged = [
            "let h = std::thread::spawn(move || run());",
            "    thread::spawn(f);",
            "std::thread::scope(|s| { s.spawn(|| 1); });",
            "let b = std::thread::Builder::new();",
            "use std::thread::{Builder, JoinHandle};",
            "use std::thread::spawn;",
            "builder().stack_size(4096)",
            "use std::thread::{\n    JoinHandle,\n    Builder,\n};",
        ];
        for source in flagged {
            assert!(
                !bare_thread_creations(source).is_empty(),
                "missed a bypass: {source}"
            );
        }
        let spared = [
            "harn_parser::runtime_stack::spawn(move || run());",
            "runtime_stack::scope(|scope| { scope.spawn(|| 1); });",
            "runtime_stack::builder().name(n).spawn(f)",
            "tokio::runtime::Builder::new_multi_thread().thread_stack_size(RUNTIME_STACK_SIZE)",
            "tokio::task::spawn_blocking(f);",
            "use std::thread::{JoinHandle, ScopedJoinHandle};",
            "let id = std::thread::current().id(); // not std::thread::spawn here",
        ];
        for source in spared {
            assert!(
                bare_thread_creations(source).is_empty(),
                "flagged a sanctioned form: {source}"
            );
        }
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
