//! Integration coverage for the on-disk bytecode cache wired into
//! `harn run`. Verifies the three invariants the cache must preserve:
//!
//! 1. Edits to the entry pipeline source bust the cache.
//! 2. Edits to any transitively-imported user file bust the cache.
//! 3. `harn precompile` produces relocatable artifacts that `harn run`
//!    recognizes, skipping recompile while preserving load-site diagnostics.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;

use harn_cli::cli::PrecompileArgs;
use harn_cli::commands::precompile;
use harn_cli::commands::run::{execute_run, CliLlmMockMode, RunOutcome, RunProfileOptions};
use harn_cli::tests::common::{cwd_lock, harn_state_lock};
use tempfile::TempDir;

fn run_in_harn_runtime<F, Fut, R>(future_factory: F) -> R
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = R>,
    R: Send + 'static,
{
    let handle = thread::Builder::new()
        .name("harn-cli-bytecache-test".to_string())
        .stack_size(harn_cli::CLI_RUNTIME_STACK_SIZE)
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("build runtime");
            runtime.block_on(future_factory())
        })
        .expect("spawn runtime thread");
    handle.join().expect("runtime thread completed")
}

fn run_harn(cache_dir: &Path, script: &Path) -> RunOutcome {
    let cache_dir = cache_dir.to_path_buf();
    let script = script.to_path_buf();
    run_in_harn_runtime(move || async move {
        let _env_guard = harn_state_lock::lock_harn_state_async().await;
        let _cwd_guard = cwd_lock::lock_cwd_async().await;
        harn_vm::reset_thread_local_state();
        let prev_cache = std::env::var("HARN_CACHE_DIR").ok();
        std::env::set_var("HARN_CACHE_DIR", &cache_dir);
        let outcome = execute_run(
            &script.to_string_lossy(),
            false,
            HashSet::new(),
            Vec::new(),
            Vec::new(),
            CliLlmMockMode::Off,
            None,
            RunProfileOptions::default(),
        )
        .await;
        match prev_cache {
            Some(value) => std::env::set_var("HARN_CACHE_DIR", value),
            None => std::env::remove_var("HARN_CACHE_DIR"),
        }
        outcome
    })
}

/// Only used to enrich a failure message: the assertions themselves name the
/// artifact they expect rather than counting the shared directory.
fn module_cache_entries(dir: &Path) -> Vec<PathBuf> {
    cache_entries_with_extension(dir, "harnmod")
}

/// The entry-chunk artifact `source_path` compiles to.
///
/// `HARN_CACHE_DIR` is process-global, so while a test holds it pointed at its
/// own `TempDir`, every other test compiling in parallel writes artifacts there
/// too. Naming the expected artifact keeps these assertions about what this
/// test compiled; counting the directory would describe the suite's scheduling.
fn expected_entry_artifact(cache_dir: &Path, source_path: &Path) -> PathBuf {
    let source = fs::read_to_string(source_path).expect("read entry source");
    let key = harn_vm::bytecode_cache::CacheKey::from_source(source_path, &source);
    cache_dir.join(key.filename())
}

/// The module artifact `source_path` compiles to. See
/// [`expected_entry_artifact`]. The module key is deliberately
/// path-independent — identical source and compiler inputs share one
/// relocatable artifact — so only the text is hashed.
fn expected_module_artifact(cache_dir: &Path, source_path: &Path) -> PathBuf {
    let source = fs::read_to_string(source_path).expect("read module source");
    let key = harn_vm::bytecode_cache::CacheKey::from_module_source(
        &harn_vm::module_source::ModuleSource::from_text(source),
    );
    cache_dir.join(key.module_filename())
}

fn cache_entries_with_extension(dir: &Path, ext: &str) -> Vec<PathBuf> {
    let ext = ext.to_string();
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)
        .map(|read| {
            read.filter_map(|entry| entry.ok())
                .map(|entry| entry.path())
                .filter(|path| path.extension().is_some_and(|e| e == ext.as_str()))
                .collect()
        })
        .unwrap_or_default();
    entries.sort();
    entries
}

#[test]
fn source_edit_invalidates_cache() {
    let workdir = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    let script = workdir.path().join("entry.harn");
    fs::write(&script, "__io_println(\"alpha\")\n").unwrap();

    let first = run_harn(cache.path(), &script);
    assert_eq!(first.exit_code, 0, "first run failed: {}", first.stderr);
    assert!(first.stdout.contains("alpha"), "stdout: {:?}", first.stdout);
    assert!(
        expected_entry_artifact(cache.path(), &script).is_file(),
        "expected this script's cache entry to be written"
    );

    fs::write(&script, "__io_println(\"bravo\")\n").unwrap();
    let second = run_harn(cache.path(), &script);
    assert_eq!(second.exit_code, 0, "second run failed: {}", second.stderr);
    assert!(
        second.stdout.contains("bravo"),
        "stdout: {:?}",
        second.stdout
    );
    // Old + new entry coexist because the cache filename is keyed by source
    // hash. Confirm the edited content got its own entry, by name.
    assert!(
        expected_entry_artifact(cache.path(), &script).is_file(),
        "edited script should get its own cache entry"
    );
}

#[test]
fn imported_module_is_cached_to_disk() {
    let workdir = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    let lib = workdir.path().join("lib.harn");
    fs::write(&lib, "pub fn answer() -> int { return 42 }\n").unwrap();
    let script = workdir.path().join("entry.harn");
    fs::write(
        &script,
        "import { answer } from \"./lib\"\n__io_println(answer())\n",
    )
    .unwrap();

    let first = run_harn(cache.path(), &script);
    assert_eq!(first.exit_code, 0, "first run failed: {}", first.stderr);
    assert!(first.stdout.contains("42"), "stdout: {:?}", first.stdout);
    let lib_artifact = expected_module_artifact(cache.path(), &lib);
    assert!(
        lib_artifact.is_file(),
        "imported lib should have produced its cached module artifact at {}, dir held {:?}",
        lib_artifact.display(),
        module_cache_entries(cache.path())
    );

    let second = run_harn(cache.path(), &script);
    assert_eq!(second.exit_code, 0, "second run failed: {}", second.stderr);
    assert!(second.stdout.contains("42"));
    // The artifact name is the compilation key, so a stable name across runs is
    // what makes the second run a hit rather than a miss under a new key.
    // Deliberately not an mtime comparison: mtime granularity is one second on
    // some filesystems, so "unchanged" would pass without proving anything.
    // Reuse-instead-of-recompile is covered by `precompile_then_run_skips_compile`.
    assert!(
        lib_artifact.is_file(),
        "second run should resolve the same cache key, expected {}",
        lib_artifact.display()
    );
}

#[test]
fn imported_file_edit_invalidates_cache() {
    let workdir = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    let lib = workdir.path().join("lib.harn");
    fs::write(&lib, "pub fn greet() -> string { return \"lib v1\" }\n").unwrap();
    let script = workdir.path().join("entry.harn");
    fs::write(
        &script,
        "import { greet } from \"./lib\"\n__io_println(greet())\n",
    )
    .unwrap();

    let first = run_harn(cache.path(), &script);
    assert_eq!(first.exit_code, 0, "first run failed: {}", first.stderr);
    assert!(
        first.stdout.contains("lib v1"),
        "stdout: {:?}",
        first.stdout
    );

    fs::write(&lib, "pub fn greet() -> string { return \"lib v2\" }\n").unwrap();
    let second = run_harn(cache.path(), &script);
    assert_eq!(second.exit_code, 0, "second run failed: {}", second.stderr);
    assert!(
        second.stdout.contains("lib v2"),
        "expected the cached entry to be invalidated after the import \
         changed; got stdout: {:?}",
        second.stdout
    );
}

#[test]
fn precompile_then_run_skips_compile() {
    let workdir = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    let script = workdir.path().join("entry.harn");
    fs::write(&script, "__io_println(\"precompiled\")\n").unwrap();

    run_in_harn_runtime({
        let target = script.clone();
        move || async move {
            let _env_guard = harn_state_lock::lock_harn_state_async().await;
            harn_vm::reset_thread_local_state();
            // Call the in-process Rust path directly. The async dispatch
            // wrapper (`precompile::run`) spawns child processes which
            // wouldn't see the in-process state these tests are pinning.
            precompile::run_inner_compile(PrecompileArgs {
                target,
                out: None,
                keep_going: false,
                quiet: true,
            });
        }
    });

    let adjacent = workdir.path().join("entry.harnbc");
    assert!(
        adjacent.exists(),
        "expected precompile to produce {adjacent:?}"
    );
    let metadata = fs::metadata(&adjacent).expect("read adjacent metadata");
    assert!(metadata.len() > 0, "precompiled artifact is empty");
    let module_adjacent = workdir.path().join("entry.harnmod");
    assert!(
        module_adjacent.exists(),
        "expected precompile to also produce module artifact {module_adjacent:?} so files imported \
         elsewhere hit the cache without recompile"
    );
    assert!(
        fs::metadata(&module_adjacent).unwrap().len() > 0,
        "precompiled module artifact is empty"
    );

    let run_result = run_harn(cache.path(), &script);
    assert_eq!(run_result.exit_code, 0, "run failed: {}", run_result.stderr);
    assert!(
        run_result.stdout.contains("precompiled"),
        "stdout: {:?}",
        run_result.stdout
    );
    // Adjacent artifact was the cache source, so nothing for THIS script
    // reaches the shared dir.
    let shared = expected_entry_artifact(cache.path(), &script);
    assert!(
        !shared.exists(),
        "expected adjacent artifact to satisfy the loader without populating \
         the shared cache dir; got {shared:?}"
    );
}

#[test]
fn relocated_precompiled_module_uses_adjacent_artifact_and_rebinds_diagnostics() {
    let build_root = TempDir::new().unwrap();
    let run_root = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    let build_lib = build_root.path().join("lib.harn");
    fs::write(
        &build_lib,
        "pub fn fail_after_relocation() {\n  throw \"relocated boom\"\n}\n",
    )
    .unwrap();

    run_in_harn_runtime({
        let target = build_lib.clone();
        move || async move {
            let _env_guard = harn_state_lock::lock_harn_state_async().await;
            harn_vm::reset_thread_local_state();
            // Call the in-process Rust path directly. The async dispatch
            // wrapper (`precompile::run`) spawns child processes which
            // wouldn't see the in-process state these tests are pinning.
            precompile::run_inner_compile(PrecompileArgs {
                target,
                out: None,
                keep_going: false,
                quiet: true,
            });
        }
    });

    let build_artifact = build_root.path().join("lib.harnmod");
    assert!(
        build_artifact.exists(),
        "expected precompile to produce imported module artifact {build_artifact:?}"
    );

    let run_lib = run_root.path().join("lib.harn");
    let run_artifact = run_root.path().join("lib.harnmod");
    fs::rename(&build_lib, &run_lib).unwrap();
    fs::rename(&build_artifact, &run_artifact).unwrap();
    let script = run_root.path().join("entry.harn");
    fs::write(
        &script,
        "import { fail_after_relocation } from \"./lib\"\nfail_after_relocation()\n",
    )
    .unwrap();

    let run_result = run_harn(cache.path(), &script);
    assert_ne!(run_result.exit_code, 0, "runtime throw should fail");
    assert!(
        run_result.stderr.contains("relocated boom"),
        "stderr: {}",
        run_result.stderr
    );
    let run_lib_display =
        harn_parser::diagnostic::normalize_diagnostic_path(&run_lib.to_string_lossy());
    assert!(
        run_result.stderr.contains(&run_lib_display),
        "relocated artifact must attribute the runtime error to its load site {run_lib_display}; \
         stderr: {}",
        run_result.stderr
    );
    let build_lib_display =
        harn_parser::diagnostic::normalize_diagnostic_path(&build_lib.to_string_lossy());
    assert!(
        !run_result.stderr.contains(&build_lib_display),
        "relocated artifact leaked its build-time path {build_lib_display}; stderr: {}",
        run_result.stderr
    );
    let shared_artifact = expected_module_artifact(cache.path(), &run_lib);
    assert!(
        !shared_artifact.exists(),
        "relocated adjacent .harnmod should satisfy the import without a shared module-cache \
         write, but {} was written",
        shared_artifact.display()
    );
}

#[test]
fn disabled_cache_does_not_write_files() {
    let workdir = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    let script = workdir.path().join("entry.harn");
    fs::write(&script, "__io_println(\"no cache\")\n").unwrap();

    let cache_dir = cache.path().to_path_buf();
    let script_for_run = script.clone();
    let outcome = run_in_harn_runtime(move || async move {
        let script = script_for_run;
        let _env_guard = harn_state_lock::lock_harn_state_async().await;
        let _cwd_guard = cwd_lock::lock_cwd_async().await;
        harn_vm::reset_thread_local_state();
        let prev_cache = std::env::var("HARN_CACHE_DIR").ok();
        let prev_toggle = std::env::var("HARN_BYTECODE_CACHE").ok();
        std::env::set_var("HARN_CACHE_DIR", &cache_dir);
        std::env::set_var("HARN_BYTECODE_CACHE", "0");
        let outcome = execute_run(
            &script.to_string_lossy(),
            false,
            HashSet::new(),
            Vec::new(),
            Vec::new(),
            CliLlmMockMode::Off,
            None,
            RunProfileOptions::default(),
        )
        .await;
        match prev_cache {
            Some(value) => std::env::set_var("HARN_CACHE_DIR", value),
            None => std::env::remove_var("HARN_CACHE_DIR"),
        }
        match prev_toggle {
            Some(value) => std::env::set_var("HARN_BYTECODE_CACHE", value),
            None => std::env::remove_var("HARN_BYTECODE_CACHE"),
        }
        outcome
    });

    assert_eq!(outcome.exit_code, 0, "stderr: {}", outcome.stderr);
    assert!(outcome.stdout.contains("no cache"));
    assert!(
        !expected_entry_artifact(cache.path(), &script).exists(),
        "cache disabled should not write this script's entry"
    );
}
