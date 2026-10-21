use harn_vm_core::vm::{
    reset_stdlib_cache_for_test as reset_stdlib_module_artifact_cache,
    stdlib_cached_ptr_for_test as cached_stdlib_module_ptr,
};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
static CACHE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
fn cache_test_guard() -> MutexGuard<'static, ()> {
    CACHE_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
}

#[test]
fn stdlib_artifact_cache_reuses_compilation_with_fresh_vm_state() {
    let _guard = cache_test_guard();
    reset_stdlib_module_artifact_cache();
    let runtime = crate::test_support::current_thread_runtime();

    let (first_exports, second_exports, first_state_weak, second_state_weak) =
        runtime.block_on(async {
            let mut first_vm = crate::new_vm();
            let first_exports = first_vm
                .load_module_exports_from_import("std/agent/prompts")
                .await
                .expect("first stdlib import succeeds");
            let first_state = first_exports
                .get("render_agent_prompt")
                .expect("first export exists")
                .module_state_for_test()
                .expect("first module state stays live while VM owns module");
            let first_state_weak = Arc::downgrade(&first_state);
            let first_state_ptr = Arc::as_ptr(&first_state);

            let mut second_vm = crate::new_vm();
            let second_exports = second_vm
                .load_module_exports_from_import("std/agent/prompts")
                .await
                .expect("second stdlib import succeeds");
            let second_state = second_exports
                .get("render_agent_prompt")
                .expect("second export exists")
                .module_state_for_test()
                .expect("second module state stays live while VM owns module");
            let second_state_weak = Arc::downgrade(&second_state);

            assert_ne!(first_state_ptr, Arc::as_ptr(&second_state));
            (
                first_exports,
                second_exports,
                first_state_weak,
                second_state_weak,
            )
        });
    let first_cached =
        cached_stdlib_module_ptr("agent/prompts").expect("first import cached stdlib artifact");
    assert_eq!(
        cached_stdlib_module_ptr("agent/prompts"),
        Some(first_cached)
    );

    let first = first_exports
        .get("render_agent_prompt")
        .expect("first export exists");
    let second = second_exports
        .get("render_agent_prompt")
        .expect("second export exists");

    assert!(!Arc::ptr_eq(first, second));
    assert!(Arc::ptr_eq(&first.func, &second.func));
    assert!(Arc::ptr_eq(&first.func.chunk, &second.func.chunk));
    assert!(first.module_state_for_test().is_none());
    assert!(second.module_state_for_test().is_none());
    assert!(first_state_weak.upgrade().is_none());
    assert!(second_state_weak.upgrade().is_none());
}

#[test]
fn warm_stdlib_disk_hit_resolves_no_imported_interface() {
    // The regression this pins: deriving a stdlib module's cache identity used
    // to require resolving its imported interface, which lexes and parses the
    // module. That ran ahead of the lookup, so every warm process re-parsed the
    // whole stdlib closure to compute the key for artifacts it then loaded from
    // disk without parsing anything.
    let _guard = cache_test_guard();
    let cache = tempfile::tempdir().expect("temp cache dir");
    let previous = std::env::var_os(crate::bytecode_cache::CACHE_DIR_ENV);
    std::env::set_var(crate::bytecode_cache::CACHE_DIR_ENV, cache.path());

    let runtime = crate::test_support::current_thread_runtime();
    let load_once = || {
        reset_stdlib_module_artifact_cache();
        runtime.block_on(async {
            crate::new_vm()
                .load_module_exports_from_import("std/agent/prompts")
                .await
                .expect("stdlib import succeeds");
        });
    };
    let resolutions_during = |load: &dyn Fn()| {
        let before = harn_vm_core::vm::module_interface_resolution_count_for_test();
        load();
        harn_vm_core::vm::module_interface_resolution_count_for_test() - before
    };

    // Cold: nothing is on disk yet, so the compile needs the interface. This
    // arm is the counter's positive control — without it a seam that never
    // increments would pass the warm assertion vacuously.
    let cold = resolutions_during(&load_once);
    assert!(
        cold > 0,
        "a cold stdlib load must resolve the interface it compiles against"
    );

    // Warm: same sources, same key, artifacts now on disk. Resetting the
    // in-process artifact cache forces the lookup to go back to disk.
    let warm = resolutions_during(&load_once);
    assert_eq!(
        warm, 0,
        "a warm stdlib load must not parse the modules it loads from disk"
    );

    match previous {
        Some(value) => std::env::set_var(crate::bytecode_cache::CACHE_DIR_ENV, value),
        None => std::env::remove_var(crate::bytecode_cache::CACHE_DIR_ENV),
    }
}

#[test]
fn stdlib_artifact_cache_is_process_wide_across_threads() {
    let _guard = cache_test_guard();
    reset_stdlib_module_artifact_cache();

    let handle = crate::runtime_stack::builder()
        .name("stdlib-cache-probe".to_string())
        .spawn(|| {
            let runtime = crate::test_support::current_thread_runtime();
            runtime.block_on(async {
                let mut vm = crate::new_vm();
                vm.load_module_exports_from_import("std/agent/prompts")
                    .await
                    .expect("thread stdlib import succeeds");
            });
        })
        .expect("spawn stdlib cache probe");
    handle.join().expect("thread joins");
    let thread_cached =
        cached_stdlib_module_ptr("agent/prompts").expect("thread import cached stdlib artifact");

    let runtime = crate::test_support::current_thread_runtime();
    runtime.block_on(async {
        let mut vm = crate::new_vm();
        vm.load_module_exports_from_import("std/agent/prompts")
            .await
            .expect("main-thread stdlib import succeeds");
    });
    assert_eq!(
        cached_stdlib_module_ptr("agent/prompts"),
        Some(thread_cached)
    );
}

#[test]
fn warm_embedded_stdlib_leaves_the_agent_stack_on_disk() {
    // harn#8575: a conformance case's `harn run` child compiled the agent
    // stack itself, inside the case deadline, alongside every sibling doing
    // the same. The suite now warms the stdlib first; a module the warm skips
    // is compiled in each child again, so every module must warm and the
    // child's imports must then come from disk. The cold arm of
    // `warm_stdlib_disk_hit_resolves_no_imported_interface` proves the counter.
    let _guard = cache_test_guard();
    let cache = tempfile::tempdir().expect("temp cache dir");
    let previous = std::env::var_os(crate::bytecode_cache::CACHE_DIR_ENV);
    std::env::set_var(crate::bytecode_cache::CACHE_DIR_ENV, cache.path());
    reset_stdlib_module_artifact_cache();

    let report = crate::vm::warm_embedded_stdlib(4);
    assert_eq!(report.modules, harn_modules::stdlib_catalog().sources.len());
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    assert_eq!(report.warmed, report.modules);
    // The warm held the cache's lock and released it on return.
    let lock_path = crate::bytecode_cache::cache_dir()
        .expect("cache dir resolves")
        .join(harn_vm_core::vm::stdlib_warm_lock_file_for_test());
    std::fs::File::open(&lock_path)
        .expect("the warm created its lock file")
        .try_lock()
        .expect("the warm released its lock");

    reset_stdlib_module_artifact_cache();
    let runtime = crate::test_support::current_thread_runtime();
    let before = harn_vm_core::vm::module_interface_resolution_count_for_test();
    runtime.block_on(async {
        let mut vm = crate::new_vm();
        crate::register_vm_stdlib(&mut vm);
        vm.load_module_exports_from_import("std/agent/loop")
            .await
            .expect("stdlib import succeeds");
    });
    let resolved = harn_vm_core::vm::module_interface_resolution_count_for_test() - before;
    assert_eq!(
        resolved, 0,
        "a warmed stdlib import must not compile anything"
    );

    match previous {
        Some(value) => std::env::set_var(crate::bytecode_cache::CACHE_DIR_ENV, value),
        None => std::env::remove_var(crate::bytecode_cache::CACHE_DIR_ENV),
    }
}
