#![recursion_limit = "256"]

//! Integration coverage for the on-disk bytecode cache wired into
//! `harn run`. Verifies the three invariants the cache must preserve:
//!
//! 1. Edits to the entry pipeline source bust the cache.
//! 2. Edits to any transitively-imported user file bust the cache.
//! 3. `harn precompile` produces artifacts that `harn run` recognizes,
//!    skipping recompile on subsequent runs.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;

use harn_cli::cli::PrecompileArgs;
use harn_cli::commands::precompile;
use harn_cli::commands::run::{execute_run, CliLlmMockMode, RunOutcome, RunProfileOptions};
use harn_cli::tests::common::{cwd_lock, env_lock};
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
        let _env_guard = env_lock::lock_env().lock().await;
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

fn cache_entries(dir: &Path) -> Vec<PathBuf> {
    cache_entries_with_extension(dir, "harnbc")
}

fn module_cache_entries(dir: &Path) -> Vec<PathBuf> {
    cache_entries_with_extension(dir, "harnmod")
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
    assert_eq!(
        cache_entries(cache.path()).len(),
        1,
        "expected one cache entry"
    );

    fs::write(&script, "__io_println(\"bravo\")\n").unwrap();
    let second = run_harn(cache.path(), &script);
    assert_eq!(second.exit_code, 0, "second run failed: {}", second.stderr);
    assert!(
        second.stdout.contains("bravo"),
        "stdout: {:?}",
        second.stdout
    );
    // Old + new entry coexist because the cache filename is keyed by
    // source hash. We just need to confirm the new content shows up.
    let entries = cache_entries(cache.path());
    assert!(
        entries.len() >= 2,
        "expected ≥2 cache entries, got {entries:?}"
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
    let module_entries = module_cache_entries(cache.path());
    assert_eq!(
        module_entries.len(),
        1,
        "imported lib should have produced one cached module artifact, got {module_entries:?}"
    );

    let second = run_harn(cache.path(), &script);
    assert_eq!(second.exit_code, 0, "second run failed: {}", second.stderr);
    assert!(second.stdout.contains("42"));
    assert_eq!(
        module_cache_entries(cache.path()).len(),
        1,
        "second run should reuse the cached module without writing a new artifact"
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
            let _env_guard = env_lock::lock_env().lock().await;
            harn_vm::reset_thread_local_state();
            // Call the in-process Rust path directly. The async dispatch
            // wrapper (`precompile::run`) spawns child processes which
            // wouldn't see the in-process state these tests are pinning.
            precompile::run_legacy(PrecompileArgs {
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
    // Adjacent artifact was the cache source, so the shared cache dir
    // stays empty.
    let shared = cache_entries(cache.path());
    assert!(
        shared.is_empty(),
        "expected adjacent artifact to satisfy the loader without populating \
         the shared cache dir; got {shared:?}"
    );
}

#[test]
fn precompiled_imported_module_uses_adjacent_artifact() {
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

    run_in_harn_runtime({
        let target = lib;
        move || async move {
            let _env_guard = env_lock::lock_env().lock().await;
            harn_vm::reset_thread_local_state();
            // Call the in-process Rust path directly. The async dispatch
            // wrapper (`precompile::run`) spawns child processes which
            // wouldn't see the in-process state these tests are pinning.
            precompile::run_legacy(PrecompileArgs {
                target,
                out: None,
                keep_going: false,
                quiet: true,
            });
        }
    });

    let module_adjacent = workdir.path().join("lib.harnmod");
    assert!(
        module_adjacent.exists(),
        "expected precompile to produce imported module artifact {module_adjacent:?}"
    );

    let run_result = run_harn(cache.path(), &script);
    assert_eq!(run_result.exit_code, 0, "run failed: {}", run_result.stderr);
    assert!(run_result.stdout.contains("42"));
    assert!(
        module_cache_entries(cache.path()).is_empty(),
        "expected adjacent .harnmod to satisfy the import without writing a shared module cache"
    );
}

#[test]
fn disabled_cache_does_not_write_files() {
    let workdir = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    let script = workdir.path().join("entry.harn");
    fs::write(&script, "__io_println(\"no cache\")\n").unwrap();

    let cache_dir = cache.path().to_path_buf();
    let outcome = run_in_harn_runtime(move || async move {
        let _env_guard = env_lock::lock_env().lock().await;
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
        cache_entries(cache.path()).is_empty(),
        "cache disabled should leave the dir untouched"
    );
}
