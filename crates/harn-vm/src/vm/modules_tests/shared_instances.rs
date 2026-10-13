use super::*;

fn stdlib_root() -> Vm {
    let mut vm = Vm::new();
    crate::stdlib::register_vm_stdlib(&mut vm);
    vm
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime builds")
}

fn stdlib_path(module: &str) -> PathBuf {
    PathBuf::from(format!("<stdlib>/{module}.harn"))
}

fn leaked(source: &str) -> &'static str {
    Box::leak(source.to_string().into_boxed_str())
}

async fn call_export(vm: &mut Vm, loaded: &LoadedModule, name: &str) -> VmValue {
    let closure = loaded
        .functions
        .get(name)
        .cloned()
        .unwrap_or_else(|| panic!("module exports `{name}`"));
    vm.call_closure_pub(&closure, &[])
        .await
        .expect("export call succeeds")
}

#[test]
fn harn_entry_crossings_instantiate_an_immutable_stdlib_closure_once() {
    // The text-tool parse seam crosses into `std/llm/tool_parse` once per
    // tool-bearing LLM response, and no stdlib module imports it, so the
    // parent VM never holds it warm. Every crossing used to instantiate its
    // whole closure into a transient child and drop it.
    let ctx = crate::vm::AsyncBuiltinCtx::for_test(stdlib_root());
    let runtime = runtime();
    const CROSSINGS: usize = 6;
    for _ in 0..CROSSINGS {
        let parsed = runtime
            .block_on(crate::llm::api::parse_text_tools_with_harn(
                Some(&ctx),
                "<assistant_prose>Reading the file now.</assistant_prose>",
                None,
                "",
            ))
            .expect("text-tool parse crossing succeeds");
        assert!(parsed.prose.contains("Reading the file now."));
        let version = runtime
            .block_on(crate::stdlib::harn_entry::call_harn_export_by_name(
                &ctx,
                "std/semver",
                "parse",
                "test",
                &[VmValue::string("v1.2.3")],
            ))
            .expect("semver crossing succeeds");
        assert!(version.as_dict().is_some());
    }

    let probe = ctx.child_vm();
    for module in ["llm/tool_parse", "llm/dialects", "semver"] {
        let path = stdlib_path(module);
        assert!(
            !probe.module_cache.contains_key(&path),
            "the parent must stay cold for {module}, or this measures the warm path"
        );
        assert_eq!(
            probe.shared_module_instances.instantiation_count(&path),
            1,
            "{module} must instantiate once across {CROSSINGS} crossings"
        );
    }
}

#[test]
fn a_module_with_mutable_top_level_state_stays_fresh_per_cold_crossing() {
    // Negative control: `std/llm/handlers` keeps a mutable circuit memo, so
    // reusing its instance would carry state between crossings that today
    // each see a fresh module. It must keep instantiating per crossing while
    // its immutable imports are still shared.
    let ctx = crate::vm::AsyncBuiltinCtx::for_test(stdlib_root());
    let runtime = runtime();
    const CROSSINGS: usize = 3;
    for _ in 0..CROSSINGS {
        let key = runtime
            .block_on(crate::stdlib::harn_entry::call_harn_export_by_name(
                &ctx,
                "std/llm/handlers",
                "llm_cache_key",
                "test",
                &[VmValue::string("prompt")],
            ))
            .expect("handlers crossing succeeds");
        assert!(matches!(key, VmValue::String(_)));
    }

    let probe = ctx.child_vm();
    assert_eq!(
        probe
            .shared_module_instances
            .instantiation_count(&stdlib_path("llm/handlers")),
        CROSSINGS
    );
    assert_eq!(
        probe
            .shared_module_instances
            .instantiation_count(&stdlib_path("math")),
        1,
        "an immutable import of a mutable module is still shared"
    );

    // `std/agent/loop` reaches `std/llm/handlers`, so it is never shared.
    runtime.block_on(async {
        for _ in 0..2 {
            let mut child = ctx.child_vm();
            child
                .load_module_exports_from_import("std/agent/loop")
                .await
                .expect("agent loop loads");
        }
    });
    assert_eq!(
        probe
            .shared_module_instances
            .instantiation_count(&stdlib_path("agent/loop")),
        2
    );
}

#[test]
fn shared_instances_follow_source_identity_and_preserve_mutable_state_isolation() {
    let runtime = runtime();
    runtime.block_on(async {
        let root = stdlib_root();
        let synthetic = stdlib_path("shared_instance_probe");
        let v1 = leaked("pub fn version() -> int {\n  return 1\n}\n");
        let v2 = leaked("pub fn version() -> int {\n  return 2\n}\n");

        let mut first = root.child_vm();
        let a = first
            .load_stdlib_module_from_source("shared_instance_probe", synthetic.clone(), v1)
            .await
            .expect("first load");
        assert_eq!(
            call_export(&mut first, &a, "version").await.as_int(),
            Some(1)
        );

        let mut second = root.child_vm();
        let b = second
            .load_stdlib_module_from_source("shared_instance_probe", synthetic.clone(), v1)
            .await
            .expect("second load");
        assert!(Arc::ptr_eq(&a, &b), "identical source reuses the instance");

        let mut edited = root.child_vm();
        let c = edited
            .load_stdlib_module_from_source("shared_instance_probe", synthetic.clone(), v2)
            .await
            .expect("edited load");
        assert!(!Arc::ptr_eq(&a, &c), "edited source must re-instantiate");
        assert_eq!(
            call_export(&mut edited, &c, "version").await.as_int(),
            Some(2)
        );
        assert_eq!(
            root.shared_module_instances.instantiation_count(&synthetic),
            2
        );

        // A fresh root shares nothing with this execution tree.
        let isolated = stdlib_root();
        assert!(isolated.shared_module_instance(&synthetic, v1).is_none());

        // Mutable top-level state is never shared: each cold child keeps
        // its own counter, exactly as before the store existed.
        let counter_path = stdlib_path("shared_instance_counter");
        let counter = leaked(
            "let count = 0\n\npub fn bump() -> int {\n  count = count + 1\n  return count\n}\n",
        );
        for _ in 0..2 {
            let mut child = root.child_vm();
            let loaded = child
                .load_stdlib_module_from_source(
                    "shared_instance_counter",
                    counter_path.clone(),
                    counter,
                )
                .await
                .expect("counter load");
            assert!(child
                .shared_module_instance(&counter_path, counter)
                .is_none());
            assert_eq!(
                call_export(&mut child, &loaded, "bump").await.as_int(),
                Some(1)
            );
        }
        assert_eq!(
            root.shared_module_instances
                .instantiation_count(&counter_path),
            2
        );
    });
}
