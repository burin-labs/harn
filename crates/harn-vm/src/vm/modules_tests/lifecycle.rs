use super::*;

#[test]
fn prepared_module_initializer_can_call_a_private_function() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime builds");
    let temp = tempfile::tempdir().expect("tempdir");
    let module = temp.path().join("initialized.harn");
    std::fs::write(
        &module,
        r"
fn initialize() {
  return 41
}

let initialized = initialize()

pub fn value() {
  return initialized
}
",
    )
    .expect("write module");
    let cache = crate::PreparedModuleCache::default();

    runtime.block_on(async {
        let mut vm = Vm::new();
        vm.set_prepared_module_cache(cache);
        let exports = vm
            .load_module_exports(&module)
            .await
            .expect("module initialization can call retained private functions");
        let value = exports.get("value").expect("public function");
        assert!(matches!(
            vm.call_closure_pub(value, &[]).await,
            Ok(VmValue::Int(41))
        ));
    });
}

#[test]
fn failed_module_initializer_restores_the_calling_vm_scope() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime builds");

    runtime.block_on(async {
        let mut vm = Vm::new();
        vm.env
            .define("caller_value", VmValue::Int(7), false)
            .expect("caller binding");
        let caller_dir = PathBuf::from("<test>/caller");
        vm.source_dir = Some(caller_dir.clone());
        let caller_permit = vm
            .sync_runtime
            .acquire("mutex", "v:caller", 1, 1, None, None)
            .await
            .expect("acquire caller permit")
            .expect("caller permit is available");
        vm.held_sync_guards
            .push(crate::synchronization::VmSyncHeldGuard {
                _permit: caller_permit,
                frame_depth: 1,
                env_scope_depth: 1,
            });
        vm.task_scopes.push(super::super::super::TaskScope {
            task_ids: Vec::new(),
            frame_depth: 1,
            env_scope_depth: 1,
        });

        let result = vm
            .load_module_from_source(
                PathBuf::from("<test>/broken_initializer.harn"),
                r#"
fn initialize() {
  throw {message: "private"}
}

let initialized = initialize()

pub fn value() {
  return initialized
}
"#,
            )
            .await;
        let error = match result {
            Ok(_) => panic!("initializer throw must fail module loading"),
            Err(error) => error,
        };

        assert!(matches!(error, VmError::Thrown(_)));
        assert!(matches!(vm.env.get("caller_value"), Some(VmValue::Int(7))));
        assert_eq!(vm.source_dir.as_ref(), Some(&caller_dir));
        assert_eq!(vm.held_permits_for("mutex", "v:caller"), 1);
        assert_eq!(vm.task_scopes.len(), 1);
        assert!(vm.imported_paths.is_empty());
    });
}

#[test]
fn cancelled_module_initializer_never_displaces_the_calling_vm_state() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime builds");

    runtime.block_on(async {
        let mut vm = Vm::new();
        vm.env
            .define("caller_value", VmValue::Int(9), false)
            .expect("caller binding");
        let caller_dir = PathBuf::from("<test>/caller");
        vm.source_dir = Some(caller_dir.clone());
        let (entered_tx, mut entered_rx) = tokio::sync::mpsc::unbounded_channel();
        vm.register_async_builtin("wait_in_initializer", move |_ctx, _args| {
            let entered_tx = entered_tx.clone();
            async move {
                entered_tx.send(()).expect("test receiver remains live");
                std::future::pending::<Result<VmValue, VmError>>().await
            }
        });

        {
            let load = vm.load_module_from_source(
                PathBuf::from("<test>/cancelled_initializer.harn"),
                r#"
let initialized = wait_in_initializer()

pub fn value() {
  return initialized
}
"#,
            );
            tokio::pin!(load);
            tokio::select! {
                entered = entered_rx.recv() => assert_eq!(entered, Some(())),
                result = &mut load => panic!("initializer unexpectedly completed: {result:?}"),
            }
        }

        assert!(matches!(vm.env.get("caller_value"), Some(VmValue::Int(9))));
        assert_eq!(vm.source_dir.as_ref(), Some(&caller_dir));
        assert!(vm.imported_paths.is_empty());
        assert!(vm.deferred_cyclic_imports.is_empty());
        assert!(vm.module_cache.is_empty());
    });
}

#[test]
fn failed_cyclic_initialization_does_not_publish_a_partial_module_graph() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime builds");
    let temp = tempfile::tempdir().expect("tempdir");
    let module_a = temp.path().join("a.harn");
    let module_b = temp.path().join("b.harn");
    std::fs::write(
        &module_a,
        r#"
import { b_value } from "./b"
fn initialize() { throw {message: "broken graph"} }
let initialized = initialize()
pub fn a_value() { return b_value() + initialized }
"#,
    )
    .expect("write module a");
    std::fs::write(
        &module_b,
        r#"
import { a_value } from "./a"
pub fn b_value() { return 1 }
pub fn through_a() { return a_value() }
"#,
    )
    .expect("write module b");

    runtime.block_on(async {
        let mut vm = Vm::new();
        let first = vm.load_module_exports(&module_a).await;
        assert!(matches!(first, Err(VmError::Thrown(_))));
        assert!(vm.imported_paths.is_empty());
        assert!(vm.deferred_cyclic_imports.is_empty());
        assert!(vm.module_cache.is_empty());

        let second = vm.load_module_exports(&module_b).await;
        assert!(matches!(second, Err(VmError::Thrown(_))));
        assert!(vm.imported_paths.is_empty());
        assert!(vm.deferred_cyclic_imports.is_empty());
        assert!(vm.module_cache.is_empty());
    });
}
