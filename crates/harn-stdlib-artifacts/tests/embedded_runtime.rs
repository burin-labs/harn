use std::path::PathBuf;

#[test]
fn embedded_agent_modules_load_under_the_native_runtime_without_compiling() {
    // Nextest gives this test its own process, including the process-global
    // catalog, embedded table and cache environment. No sibling shares them.
    let cache = tempfile::tempdir().expect("private cold cache");
    std::env::set_var("HARN_CACHE_DIR", cache.path());
    assert!(harn_vm::bytecode_cache::cache_enabled());
    harn_runtime::force_link();
    harn_stdlib::install().expect("install the shipped source catalog");
    assert!(!harn_stdlib_artifacts::ARTIFACTS.is_empty());
    harn_stdlib_artifacts::register().expect("register the build-script artifacts");

    let stats = harn_vm::on_vm_stack(|| {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("native runtime")
            .block_on(async {
                let mut vm = harn_vm::new_vm();
                harn_runtime::register_vm_stdlib(&mut vm);
                let recorder = vm.enable_module_phase_timing();
                vm.load_module_exports_from_source(
                    PathBuf::from("<test>/embedded-agent.harn"),
                    "import { agent_loop } from \"std/agent/loop\"\n\
                     import { message_10 as cancel_error } from \"std/cli/localization/models/batch_cancel/part_2\"\n\
                     import { message_10 as status_error } from \"std/cli/localization/models/batch_status/part_2\"\n\
                     assert_eq(cancel_error(\"de\", []), \"Fehler\")\n\
                     assert_eq(status_error(\"de\", []), \"Fehler\")\n\
                     pub const run = agent_loop\n",
                )
                .await
                .expect("load the real agent module graph");
                recorder.snapshot()
            })
    });
    assert!(
        stats.modules_loaded > 1,
        "the stdlib import must actually load: {stats:?}"
    );
    assert_eq!(
        stats.modules_compiled, 1,
        "only the submitted user module may compile; embedded stdlib must load: {stats:?}"
    );
}
