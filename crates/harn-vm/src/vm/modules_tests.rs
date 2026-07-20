use std::sync::{Mutex, MutexGuard, OnceLock};

use super::*;

static CACHE_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn cache_test_guard() -> MutexGuard<'static, ()> {
    CACHE_TEST_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap()
}

fn cached_stdlib_module_ptr(module: &str) -> Option<usize> {
    let source = harn_stdlib::get_stdlib_source(module).expect("stdlib module source exists");
    stdlib_module_artifact_cache_ptr(module, source)
}

#[test]
fn child_cow_module_cache_reuses_loaded_module_arcs_but_fresh_roots_do_not() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime builds");

    runtime.block_on(async {
        let primary_key = PathBuf::from("<test>/primary.harn");
        let primary_source = "pub fn primary() { return 1 }\n";
        let mut parent = Vm::new();
        let parent_loaded = parent
            .load_module_from_source(primary_key.clone(), primary_source)
            .await
            .expect("parent module loads");
        assert!(Arc::ptr_eq(
            &parent_loaded,
            parent
                .module_cache
                .get(&primary_key)
                .expect("parent cache holds primary"),
        ));

        let mut child = parent.child_vm();
        child
            .load_module_from_source(
                PathBuf::from("<test>/child-only.harn"),
                "pub fn child_only() { return 2 }\n",
            )
            .await
            .expect("child-only module loads");
        let child_loaded = child
            .load_module_from_source(primary_key.clone(), primary_source)
            .await
            .expect("child cache hit succeeds");

        assert!(Arc::ptr_eq(&parent_loaded, &child_loaded));
        assert!(Arc::ptr_eq(
            &parent_loaded,
            parent
                .module_cache
                .get(&primary_key)
                .expect("parent cache remains unchanged"),
        ));
        assert!(Arc::ptr_eq(
            &parent_loaded,
            child
                .module_cache
                .get(&primary_key)
                .expect("child COW cache retains primary"),
        ));

        let mut fresh = Vm::new();
        let fresh_loaded = fresh
            .load_module_from_source(primary_key, primary_source)
            .await
            .expect("fresh root module loads");
        assert!(
            !Arc::ptr_eq(&parent_loaded, &fresh_loaded),
            "fresh roots must still instantiate isolated runtime module state"
        );
    });
}

#[test]
fn module_phase_timing_counts_successful_unique_module_work() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime builds");

    runtime.block_on(async {
        let mut vm = Vm::new();
        let recorder = vm.enable_module_phase_timing();
        let mut child = vm.child_vm();
        let path = PathBuf::from("<test>/timed_module.harn");
        let source = "pub fn answer() { return 42 }\n";

        child
            .load_module_from_source(path.clone(), source)
            .await
            .expect("first module load succeeds");
        let first = recorder.snapshot();
        child
            .load_module_from_source(path, source)
            .await
            .expect("cached module load succeeds");

        let stats = recorder.snapshot();
        assert_eq!(stats, first, "per-VM cache hit records no module work");
        assert_eq!(stats.modules_compiled, 1);
        assert_eq!(stats.modules_loaded, 1);
    });
}

#[test]
fn module_phase_timing_does_not_count_failed_compile() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime builds");

    runtime.block_on(async {
        let mut vm = Vm::new();
        let recorder = vm.enable_module_phase_timing();

        let result = vm
            .load_module_from_source(
                PathBuf::from("<test>/invalid_timed_module.harn"),
                "pub fn broken( {",
            )
            .await;
        assert!(result.is_err(), "invalid module must fail compilation");

        let stats = recorder.snapshot();
        assert_eq!(stats.modules_compiled, 0);
        assert_eq!(stats.modules_loaded, 0);
    });
}

#[test]
fn failed_read_does_not_leak_module_counts_to_next_vm() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime builds");
    let temp = tempfile::tempdir().expect("tempdir");
    let valid = temp.path().join("valid.harn");
    std::fs::write(&valid, "pub fn answer() { return 42 }\n").expect("write valid module");

    runtime.block_on(async {
        let mut failed_vm = Vm::new();
        failed_vm.set_source_dir(temp.path());
        let failed_recorder = failed_vm.enable_module_phase_timing();

        assert!(failed_vm.execute_import("./missing", None).await.is_err());
        assert_eq!(failed_recorder.snapshot().modules_loaded, 0);
        drop(failed_vm);

        let mut next_vm = Vm::new();
        let next_recorder = next_vm.enable_module_phase_timing();
        next_vm
            .load_module_exports(&valid)
            .await
            .expect("next VM load succeeds");
        assert_eq!(next_recorder.snapshot().modules_loaded, 1);
        assert_eq!(failed_recorder.snapshot().modules_loaded, 0);
    });
}

#[test]
fn module_function_can_use_local_type_alias_as_schema_value() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime builds");

    let result = runtime.block_on(async {
        let mut vm = Vm::new();
        crate::stdlib::register_vm_stdlib(&mut vm);
        let loaded = vm
            .load_module_from_source(
                PathBuf::from("<test>/schema_alias_module.harn"),
                r#"
fn accepts_schema(schema) {
  return schema_report({name: "Ada"}, schema).ok
}

type UserShape = {name: string}

pub fn works() {
  return accepts_schema(UserShape)
}
"#,
            )
            .await
            .expect("module loads");
        let closure = Arc::clone(loaded.functions.get("works").expect("works export exists"));
        vm.call_closure_pub(&closure, &[])
            .await
            .expect("module closure executes")
    });

    assert!(matches!(result, VmValue::Bool(true)), "{result:?}");
}

#[test]
fn imported_public_struct_exports_its_runtime_constructor() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime builds");
    let temp = tempfile::tempdir().expect("tempdir");
    let types = temp.path().join("types.harn");
    let consumer = temp.path().join("consumer.harn");
    std::fs::write(&types, "pub struct Decision { allowed: bool }\n").expect("write type module");
    std::fs::write(
        &consumer,
        r#"
import { Decision } from "./types"

pub fn decide() -> Decision {
  return Decision({allowed: true})
}
"#,
    )
    .expect("write consumer module");

    runtime.block_on(async {
        let mut vm = Vm::new();
        crate::register_vm_stdlib(&mut vm);
        let exports = vm
            .load_module_exports(&consumer)
            .await
            .expect("consumer module loads");
        let decide = exports.get("decide").expect("decide export");
        let result = vm
            .call_closure_pub(decide, &[])
            .await
            .expect("imported constructor executes");

        assert_eq!(result.struct_name(), Some("Decision"));
        let fields = result.struct_fields_map().expect("struct fields");
        let Some(VmValue::Bool(allowed)) = fields.get("allowed") else {
            panic!(
                "expected bool field `allowed`, got {:?}",
                fields.get("allowed")
            );
        };
        assert!(*allowed, "expected `allowed` to be true");
    });
}

#[test]
fn imported_public_enum_exports_namespace_and_preserves_source_context() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime builds");
    let temp = tempfile::tempdir().expect("tempdir");
    let library = temp.path().join("library.harn");
    let facade = temp.path().join("facade.harn");
    let consumer = temp.path().join("consumer.harn");
    let wildcard_consumer = temp.path().join("wildcard_consumer.harn");
    std::fs::write(
        &library,
        r"
pub enum Color {
  Ready(message: string)
  Empty
}

pub fn from_library(message: string) -> Color {
  return Color.Ready(message)
}
",
    )
    .expect("write enum module");
    std::fs::write(
        &facade,
        r#"
pub import { Color, from_library } from "./library"
"#,
    )
    .expect("write enum facade");
    std::fs::write(
        &consumer,
        r#"
import { Color, from_library } from "./facade"

pub fn exercise() -> string {
  const direct = Color.Ready("direct")
  const indirect = from_library("indirect")
  match direct {
    Color.Ready(message) -> {
      match indirect {
        Color.Ready(other) -> { return message + ":" + other }
        _ -> { return "indirect-mismatch" }
      }
    }
    _ -> { return "direct-mismatch" }
  }
}
"#,
    )
    .expect("write consumer module");
    std::fs::write(
        &wildcard_consumer,
        r#"
import "./facade"

pub fn exercise() -> string {
  const direct = Color.Ready("wildcard")
  match direct {
    Color.Ready(message) -> { return message }
    _ -> { return "wildcard-mismatch" }
  }
}
"#,
    )
    .expect("write wildcard consumer module");

    runtime.block_on(async {
        let mut vm = Vm::new();
        crate::register_vm_stdlib(&mut vm);
        let exports = vm
            .load_module_exports(&consumer)
            .await
            .expect("consumer module loads");
        let exercise = exports.get("exercise").expect("exercise export");
        let result = vm
            .call_closure_pub(exercise, &[])
            .await
            .expect("imported enum namespace and function execute");

        assert!(matches!(
            result,
            VmValue::String(value) if value.as_str() == "direct:indirect"
        ));

        let wildcard_exports = vm
            .load_module_exports(&wildcard_consumer)
            .await
            .expect("wildcard consumer module loads");
        let wildcard_exercise = wildcard_exports
            .get("exercise")
            .expect("wildcard exercise export");
        let wildcard_result = vm
            .call_closure_pub(wildcard_exercise, &[])
            .await
            .expect("wildcard imported enum namespace executes");
        assert!(matches!(
            wildcard_result,
            VmValue::String(value) if value.as_str() == "wildcard"
        ));
    });
}

#[test]
fn stdlib_artifact_cache_reuses_compilation_with_fresh_vm_state() {
    let _guard = cache_test_guard();
    reset_stdlib_module_artifact_cache();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime builds");

    let (first_exports, second_exports, first_state_weak, second_state_weak) =
        runtime.block_on(async {
            let mut first_vm = Vm::new();
            let first_exports = first_vm
                .load_module_exports_from_import("std/agent/prompts")
                .await
                .expect("first stdlib import succeeds");
            let first_state = first_exports
                .get("render_agent_prompt")
                .expect("first export exists")
                .module_state()
                .expect("first module state stays live while VM owns module");
            let first_state_weak = Arc::downgrade(&first_state);
            let first_state_ptr = Arc::as_ptr(&first_state);

            let mut second_vm = Vm::new();
            let second_exports = second_vm
                .load_module_exports_from_import("std/agent/prompts")
                .await
                .expect("second stdlib import succeeds");
            let second_state = second_exports
                .get("render_agent_prompt")
                .expect("second export exists")
                .module_state()
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
    assert!(first.module_state().is_none());
    assert!(second.module_state().is_none());
    assert!(first_state_weak.upgrade().is_none());
    assert!(second_state_weak.upgrade().is_none());
}

#[test]
fn prepared_user_module_reuses_code_with_fresh_mutable_state() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime builds");
    let temp = tempfile::tempdir().expect("tempdir");
    let module = temp.path().join("counter.harn");
    std::fs::write(
        &module,
        r"
let count = 0

pub fn increment() {
  count = count + 1
  return count
}
",
    )
    .expect("write module");
    let cache = crate::PreparedModuleCache::default();

    runtime.block_on(async {
        let mut first_vm = Vm::new();
        first_vm.set_prepared_module_cache(cache.clone());
        let first_recorder = first_vm.enable_module_phase_timing();
        let first_exports = first_vm
            .load_module_exports(&module)
            .await
            .expect("first module load succeeds");
        let first = first_exports.get("increment").expect("first export");
        assert!(matches!(
            first_vm.call_closure_pub(first, &[]).await,
            Ok(VmValue::Int(1))
        ));
        assert!(matches!(
            first_vm.call_closure_pub(first, &[]).await,
            Ok(VmValue::Int(2))
        ));
        assert_eq!(first_recorder.snapshot().modules_loaded, 1);

        let mut second_vm = Vm::new();
        second_vm.set_prepared_module_cache(cache.clone());
        let second_recorder = second_vm.enable_module_phase_timing();
        let second_exports = second_vm
            .load_module_exports(&module)
            .await
            .expect("second module load succeeds");
        let second = second_exports.get("increment").expect("second export");

        assert!(!Arc::ptr_eq(first, second));
        assert!(Arc::ptr_eq(&first.func, &second.func));
        assert!(Arc::ptr_eq(&first.func.chunk, &second.func.chunk));
        assert_ne!(
            Arc::as_ptr(&first.module_state().expect("first state")),
            Arc::as_ptr(&second.module_state().expect("second state"))
        );
        assert!(matches!(
            second_vm.call_closure_pub(second, &[]).await,
            Ok(VmValue::Int(1))
        ));
        let second_phases = second_recorder.snapshot();
        assert_eq!(second_phases.module_compile_ms, 0);
        assert_eq!(second_phases.modules_compiled, 0);
        assert_eq!(second_phases.modules_loaded, 1);
    });

    let stats = cache.stats();
    assert_eq!(stats.insertions, 1);
    assert_eq!(stats.hits, 1);
    assert_eq!(stats.entries, 1);
}

#[test]
fn prepared_importer_reloads_changed_dependency() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime builds");
    let temp = tempfile::tempdir().expect("tempdir");
    let module = temp.path().join("reader.harn");
    let dependency = temp.path().join("value.harn");
    std::fs::write(
        &module,
        "import { value } from \"./value\"\npub fn read() { return value() }\n",
    )
    .expect("write importer");
    std::fs::write(&dependency, "pub fn value() { return 1 }\n").expect("write dependency");
    let cache = crate::PreparedModuleCache::default();

    runtime.block_on(async {
        let mut first_vm = Vm::new();
        first_vm.set_prepared_module_cache(cache.clone());
        let first_exports = first_vm
            .load_module_exports(&module)
            .await
            .expect("first module load succeeds");
        let first = first_exports.get("read").expect("first export");
        let first_result = first_vm.call_closure_pub(first, &[]).await;
        assert!(
            matches!(first_result, Ok(VmValue::Int(1))),
            "unexpected first dependency result: {first_result:?}"
        );

        std::fs::write(&dependency, "pub fn value() { return 2 }\n").expect("rewrite dependency");

        let mut second_vm = Vm::new();
        second_vm.set_prepared_module_cache(cache.clone());
        let second_exports = second_vm
            .load_module_exports(&module)
            .await
            .expect("second module load succeeds");
        let second = second_exports.get("read").expect("second export");
        let second_result = second_vm.call_closure_pub(second, &[]).await;
        assert!(
            matches!(second_result, Ok(VmValue::Int(2))),
            "unexpected refreshed dependency result: {second_result:?}"
        );
    });

    let stats = cache.stats();
    assert_eq!(stats.hits, 1);
    assert_eq!(stats.insertions, 3);
    assert_eq!(stats.entries, 3);
}

#[test]
fn stdlib_artifact_cache_is_process_wide_across_threads() {
    let _guard = cache_test_guard();
    reset_stdlib_module_artifact_cache();

    let handle = std::thread::spawn(|| {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime builds");
        runtime.block_on(async {
            let mut vm = Vm::new();
            vm.load_module_exports_from_import("std/agent/prompts")
                .await
                .expect("thread stdlib import succeeds");
        });
    });
    handle.join().expect("thread joins");
    let thread_cached =
        cached_stdlib_module_ptr("agent/prompts").expect("thread import cached stdlib artifact");

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime builds");
    runtime.block_on(async {
        let mut vm = Vm::new();
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
fn module_closures_release_state_after_vm_drop() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime builds");

    let (closure_weak, registry_weak, state_weak) = runtime.block_on(async {
        let mut vm = Vm::new();
        let loaded = vm
            .load_module_from_source(
                PathBuf::from("<test>/module_cycle.harn"),
                r#"
let payload = "x" * 1024

pub fn touch() {
  return len(payload)
}
"#,
            )
            .await
            .expect("module loads");
        let closure = Arc::clone(loaded.functions.get("touch").expect("touch export exists"));
        let closure_weak = Arc::downgrade(&closure);
        let registry_weak = Arc::downgrade(&loaded._module_functions);
        let state_weak = Arc::downgrade(&loaded._module_state);

        drop(closure);
        drop(loaded);
        drop(vm);

        (closure_weak, registry_weak, state_weak)
    });

    assert!(
        closure_weak.upgrade().is_none(),
        "module closure should drop with its VM"
    );
    assert!(
        registry_weak.upgrade().is_none(),
        "module function registry should drop with its VM"
    );
    assert!(
        state_weak.upgrade().is_none(),
        "module state should drop with its VM"
    );
}
