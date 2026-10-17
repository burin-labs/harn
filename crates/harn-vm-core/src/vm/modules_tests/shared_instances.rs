use super::*;

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
fn shared_instances_follow_source_identity_and_preserve_mutable_state_isolation() {
    let runtime = runtime();
    runtime.block_on(async {
        let root = super::super::super::test_runtime::new_vm();
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
        let isolated = super::super::super::test_runtime::new_vm();
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
