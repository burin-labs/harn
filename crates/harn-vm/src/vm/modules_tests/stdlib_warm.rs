use super::*;

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
    assert_eq!(report.modules, harn_stdlib::STDLIB_SOURCES.len());
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    assert_eq!(report.warmed, report.modules);

    reset_stdlib_module_artifact_cache();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime builds");
    let before = crate::module_artifact::INTERFACE_RESOLUTIONS.with(std::cell::Cell::get);
    runtime.block_on(async {
        let mut vm = Vm::new();
        crate::stdlib::register_vm_stdlib(&mut vm);
        vm.load_module_exports_from_import("std/agent/loop")
            .await
            .expect("stdlib import succeeds");
    });
    let resolved =
        crate::module_artifact::INTERFACE_RESOLUTIONS.with(std::cell::Cell::get) - before;
    assert_eq!(
        resolved, 0,
        "a warmed stdlib import must not compile anything"
    );

    match previous {
        Some(value) => std::env::set_var(crate::bytecode_cache::CACHE_DIR_ENV, value),
        None => std::env::remove_var(crate::bytecode_cache::CACHE_DIR_ENV),
    }
}
