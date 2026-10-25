//! The stdlib warm compiles every embedded module, and several call builtin
//! families that live outside `harn-vm`, so it runs against the assembled
//! runtime rather than in the crate's unit tests.

use harn_vm_core::bytecode_cache::{cache_dir, CACHE_DIR_ENV};
use harn_vm_core::vm::{
    module_interface_resolution_count_for_test, reset_stdlib_cache_for_test,
    stdlib_warm_lock_file_for_test, warm_embedded_stdlib,
};

#[test]
fn warm_embedded_stdlib_leaves_the_agent_stack_on_disk() {
    // harn#8575: a conformance case's `harn run` child compiled the agent
    // stack itself, inside the case deadline, alongside every sibling doing
    // the same. The suite now warms the stdlib first; a module the warm skips
    // is compiled in each child again, so every module must warm and the
    // child's imports must then come from disk. The cold arm of
    // `warm_stdlib_disk_hit_resolves_no_imported_interface` proves the counter.
    let cache = tempfile::tempdir().expect("temp cache dir");
    let previous = std::env::var_os(CACHE_DIR_ENV);
    std::env::set_var(CACHE_DIR_ENV, cache.path());
    harn_runtime::force_link();
    reset_stdlib_cache_for_test();

    let report = warm_embedded_stdlib(4);
    assert_eq!(report.modules, harn_modules::stdlib_catalog().sources.len());
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    assert_eq!(report.warmed, report.modules);
    // The warm held the cache's lock and released it on return.
    let lock_path = cache_dir()
        .expect("cache dir resolves")
        .join(stdlib_warm_lock_file_for_test());
    std::fs::File::open(&lock_path)
        .expect("the warm created its lock file")
        .try_lock()
        .expect("the warm released its lock");

    reset_stdlib_cache_for_test();
    let runtime = harn_clock::test_support::current_thread_runtime();
    let before = module_interface_resolution_count_for_test();
    runtime.block_on(async {
        let mut vm = harn_vm::new_vm();
        harn_runtime::register_vm_stdlib(&mut vm);
        vm.load_module_exports_from_import("std/agent/loop")
            .await
            .expect("stdlib import succeeds");
    });
    let resolved = module_interface_resolution_count_for_test() - before;
    assert_eq!(
        resolved, 0,
        "a warmed stdlib import must not compile anything"
    );

    match previous {
        Some(value) => std::env::set_var(CACHE_DIR_ENV, value),
        None => std::env::remove_var(CACHE_DIR_ENV),
    }
}
