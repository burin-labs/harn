use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Mutex, MutexGuard, OnceLock};

use super::*;

#[path = "modules_tests/export_calls.rs"]
mod export_calls;

#[path = "modules_tests/lifecycle.rs"]
mod lifecycle;
#[path = "modules_tests/shared_instances.rs"]
mod shared_instances;
#[path = "modules_tests/stdlib_warm.rs"]
mod stdlib_warm;
#[path = "modules_tests/user_interface_warm.rs"]
mod user_interface_warm;

static CACHE_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn cache_test_guard() -> MutexGuard<'static, ()> {
    CACHE_TEST_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap()
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
        let mut parent = super::super::test_runtime::new_vm();
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

        let mut fresh = super::super::test_runtime::new_vm();
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
        let mut vm = super::super::test_runtime::new_vm();
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
        let mut vm = super::super::test_runtime::new_vm();
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
        let mut failed_vm = super::super::test_runtime::new_vm();
        failed_vm.set_source_dir(temp.path());
        let failed_recorder = failed_vm.enable_module_phase_timing();

        assert!(failed_vm.execute_import("./missing", None).await.is_err());
        assert_eq!(failed_recorder.snapshot().modules_loaded, 0);
        drop(failed_vm);

        let mut next_vm = super::super::test_runtime::new_vm();
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
fn stdlib_artifact_cache_singleflights_concurrent_exact_key() {
    const WORKERS: usize = 8;

    let _guard = cache_test_guard();
    reset_stdlib_module_artifact_cache();
    let source_path = Path::new("<stdlib>/singleflight-fixture.harn");
    let artifact = Arc::new(PreparedModuleArtifact::from_cached(
        compile_module_artifact_from_source(source_path, "pub fn value() { return 1 }")
            .expect("compile immutable fixture"),
    ));
    let calls = Arc::new(AtomicUsize::new(0));
    let (owner_entered_tx, owner_entered_rx) = mpsc::sync_channel(0);
    let (release_owner_tx, release_owner_rx) = mpsc::sync_channel(0);
    let owner = {
        let artifact = Arc::clone(&artifact);
        let calls = Arc::clone(&calls);
        crate::runtime_stack::spawn(move || {
            stdlib_artifact_get_or_prepare("singleflight-fixture".to_string(), || {
                calls.fetch_add(1, Ordering::SeqCst);
                owner_entered_tx
                    .send(())
                    .expect("test coordinator observes the preparation owner");
                release_owner_rx
                    .recv()
                    .expect("test coordinator releases the preparation owner");
                Ok(artifact)
            })
            .expect("exact-key preparation succeeds")
        })
    };
    owner_entered_rx
        .recv()
        .expect("one exact-key preparation owner enters");

    let (waiter_ready_tx, waiter_ready_rx) = mpsc::sync_channel(0);
    let mut waiters = Vec::with_capacity(WORKERS - 1);
    for _ in 1..WORKERS {
        let artifact = Arc::clone(&artifact);
        let calls = Arc::clone(&calls);
        let waiter_ready_tx = waiter_ready_tx.clone();
        waiters.push(crate::runtime_stack::spawn(move || {
            waiter_ready_tx
                .send(())
                .expect("test coordinator observes a ready waiter");
            stdlib_artifact_get_or_prepare("singleflight-fixture".to_string(), || {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(artifact)
            })
            .expect("exact-key waiter succeeds")
        }));
    }
    drop(waiter_ready_tx);
    for _ in 1..WORKERS {
        waiter_ready_rx
            .recv()
            .expect("every exact-key waiter reaches the cache seam");
    }
    release_owner_tx
        .send(())
        .expect("release the exact-key preparation owner");

    let mut outcomes = vec![owner.join().expect("stdlib cache owner joins")];
    outcomes.extend(
        waiters
            .into_iter()
            .map(|handle| handle.join().expect("stdlib cache worker joins")),
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "one exact immutable stdlib key must have one preparation owner"
    );
    assert!(
        outcomes
            .iter()
            .all(|outcome| Arc::ptr_eq(&outcomes[0], outcome)),
        "all waiters must receive the owner's exact immutable artifact"
    );
}

#[test]
fn stdlib_artifact_cache_does_not_poison_a_failed_preparation() {
    let _guard = cache_test_guard();
    reset_stdlib_module_artifact_cache();
    let first = stdlib_artifact_get_or_prepare("retry-fixture".to_string(), || {
        Err(VmError::Runtime(
            "intentional preparation failure".to_string(),
        ))
    });
    assert!(first.is_err());

    let source_path = Path::new("<stdlib>/retry-fixture.harn");
    let recovered = stdlib_artifact_get_or_prepare("retry-fixture".to_string(), || {
        Ok(Arc::new(PreparedModuleArtifact::from_cached(
            compile_module_artifact_from_source(source_path, "pub fn value() { return 1 }")?,
        )))
    })
    .expect("a later owner may retry the failed exact key");
    assert_eq!(recovered.provenance, ModuleProvenance::User);
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
        let mut first_vm = super::super::test_runtime::new_vm();
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

        let mut second_vm = super::super::test_runtime::new_vm();
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
        let mut first_vm = super::super::test_runtime::new_vm();
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

        let mut second_vm = super::super::test_runtime::new_vm();
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
fn module_closures_release_state_after_vm_drop() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime builds");

    let (closure_weak, registry_weak, state_weak) = runtime.block_on(async {
        let mut vm = super::super::test_runtime::new_vm();
        let loaded = vm
            .load_module_from_source(
                PathBuf::from("<test>/module_cycle.harn"),
                r#"
let payload = "x" * 1024

pub fn touch() {
  return payload.len()
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

#[test]
fn namespace_import_binds_alias_dict_not_flattened_members() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime builds");
    let temp = tempfile::tempdir().expect("tempdir");
    let lib = temp.path().join("lib.harn");
    std::fs::write(
        &lib,
        "pub fn greet(name) { return \"hi \" + name }\npub fn other() { return 1 }\n",
    )
    .expect("write lib");

    let result = runtime.block_on(async {
        let mut vm = super::super::test_runtime::new_vm();
        vm.set_source_dir(temp.path());
        vm.execute_namespace_import_bind("./lib", "lib", None)
            .await
            .expect("namespace import binds");
        assert!(
            vm.env.get("greet").is_none(),
            "members must not flatten into caller"
        );
        let Some(VmValue::Dict(map)) = vm.env.get("lib") else {
            panic!("alias should bind a dict, got {:?}", vm.env.get("lib"));
        };
        assert!(matches!(map.get("_namespace"), Some(VmValue::String(_))));
        assert!(map.get("greet").is_some());
        assert!(map.get("other").is_some());

        // Escaping the alias forces the complete-namespace bytecode path.
        let chunk_source = r#"
import * as lib from "./lib"
const complete = lib
pipeline default(harness: Harness) {
  return complete.greet("world")
}
"#;
        let mut lexer = harn_lexer::Lexer::new(chunk_source);
        let tokens = lexer.tokenize().expect("lex");
        let mut parser = harn_parser::Parser::new(tokens);
        let program = parser.parse().expect("parse");
        let compiler = crate::Compiler::new();
        let chunk = compiler.compile(&program).expect("compile");
        let disassembly = chunk.disassemble("escaped namespace import");
        assert!(disassembly.contains("NAMESPACE_IMPORT"));
        assert!(!disassembly.contains("NAMESPACE_IMPORT_MEMBERS"));
        let mut run_vm = super::super::test_runtime::new_vm();
        run_vm.set_source_dir(temp.path());
        run_vm
            .execute(&chunk)
            .await
            .expect("execute namespace call")
    });
    assert!(
        matches!(result, VmValue::String(ref value) if value.as_str() == "hi world"),
        "unexpected result: {result:?}"
    );
}

#[test]
fn namespace_member_projection_keeps_target_complete_and_nested_dict_narrow() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime builds");
    let temp = tempfile::tempdir().expect("tempdir");
    let lib = temp.path().join("lib.harn");
    let initializer = temp.path().join("initializer.harn");
    let wrapper = temp.path().join("wrapper.harn");
    std::fs::write(
        &lib,
        "let greeting = \"hello\"\npub fn greet() { return greeting }\npub fn unused() { return 2 }\n",
    )
    .expect("write lib");
    std::fs::write(
        &initializer,
        "let initialized = \"yes\"\npub fn status() { return initialized }\n",
    )
    .expect("write initializer");
    std::fs::write(
        &wrapper,
        "import * as lib from \"./lib\"\nimport * as unused from \"./initializer\"\npub fn call() { return lib.greet() }\n",
    )
    .expect("write wrapper");

    runtime.block_on(async {
        let mut vm = super::super::test_runtime::new_vm();
        let recorder = vm.enable_module_phase_timing();
        vm.set_source_dir(temp.path());
        let exports = vm
            .load_module_exports(&wrapper)
            .await
            .expect("wrapper loads");
        let result = vm
            .call_closure_pub(exports.get("call").expect("call export"), &[])
            .await
            .expect("projected member remains callable");
        assert!(matches!(result, VmValue::String(value) if value.as_str() == "hello"));

        let wrapper_key = wrapper.canonicalize().unwrap_or(wrapper.clone());
        let loaded_wrapper = vm.module_cache.get(&wrapper_key).expect("wrapper cached");
        {
            let state = loaded_wrapper._module_state.lock();
            let Some(VmValue::Dict(namespace)) = state.get("lib") else {
                panic!("wrapper namespace is bound");
            };
            assert_eq!(
                namespace.keys().map(|key| key.as_str()).collect::<Vec<_>>(),
                vec!["_namespace", "greet"]
            );
            let Some(VmValue::Dict(unused_namespace)) = state.get("unused") else {
                panic!("unused namespace is bound");
            };
            assert_eq!(
                unused_namespace
                    .keys()
                    .map(|key| key.as_str())
                    .collect::<Vec<_>>(),
                vec!["_namespace"]
            );
        }

        let lib_key = lib.canonicalize().unwrap_or(lib.clone());
        let loaded_lib = vm.module_cache.get(&lib_key).expect("target cached");
        assert!(loaded_lib.functions.contains_key("greet"));
        assert!(
            loaded_lib.functions.contains_key("unused"),
            "target artifacts and caches remain complete"
        );

        let initializer_key = initializer.canonicalize().unwrap_or(initializer.clone());
        let loaded_initializer = vm
            .module_cache
            .get(&initializer_key)
            .expect("unused target cached");
        let status = Arc::clone(
            loaded_initializer
                .functions
                .get("status")
                .expect("unused target remains complete"),
        );
        let initialized = vm
            .call_closure_pub(&status, &[])
            .await
            .expect("unused target initialization is observable");
        assert!(matches!(initialized, VmValue::String(value) if value.as_str() == "yes"));
        assert_eq!(
            recorder.snapshot().modules_loaded,
            3,
            "wrapper and both dependencies each initialize exactly once"
        );
    });
}

#[test]
fn cyclic_namespace_import_applies_projection_after_both_modules_load() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime builds");
    let temp = tempfile::tempdir().expect("tempdir");
    let a = temp.path().join("a.harn");
    let b = temp.path().join("b.harn");
    std::fs::write(
        &a,
        r#"
import * as b from "./b"
pub fn local() { return 7 }
pub fn extra() { return 8 }
pub fn run() { return b.via_a() }
"#,
    )
    .expect("write a");
    std::fs::write(
        &b,
        r#"
import * as a from "./a"
pub fn via_a() { return a.local() }
pub fn spare() { return 9 }
"#,
    )
    .expect("write b");

    runtime.block_on(async {
        let mut vm = super::super::test_runtime::new_vm();
        vm.set_source_dir(temp.path());
        let exports = vm.load_module_exports(&a).await.expect("cycle loads");
        let result = vm
            .call_closure_pub(exports.get("run").expect("run export"), &[])
            .await
            .expect("deferred projected binding is visible");
        assert!(matches!(result, VmValue::Int(7)));

        let b_key = b.canonicalize().unwrap_or(b.clone());
        let loaded_b = vm.module_cache.get(&b_key).expect("b cached");
        let state = loaded_b._module_state.lock();
        let Some(VmValue::Dict(namespace)) = state.get("a") else {
            panic!("deferred namespace is bound");
        };
        assert_eq!(
            namespace.keys().map(|key| key.as_str()).collect::<Vec<_>>(),
            vec!["_namespace", "local"]
        );
    });
}

#[test]
fn module_loading_reuses_the_bytes_the_entry_cache_key_already_read() {
    // Computing an entry chunk's cache key walks the transitive import graph
    // and reads every module in it. The module loader must consume those same
    // bytes rather than reading each file a second time and keeping a private
    // copy: on a large graph the duplicate read and copy are paid on every
    // spawn. Pointer identity between the loader's `source_cache` entry and the
    // shared owner's bytes is the invariant that keeps them a single read.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime builds");
    let temp = tempfile::tempdir().expect("tempdir");
    let entry = temp.path().join("entry.harn");
    let dependency = temp.path().join("value.harn");
    std::fs::write(
        &entry,
        "import { value } from \"./value\"\npub fn read() { return value() }\n",
    )
    .expect("write entry");
    std::fs::write(&dependency, "pub fn value() { return 1 }\n").expect("write dependency");

    // Fold the entry key exactly as the run path does, warming the shared owner.
    let entry_source = std::fs::read_to_string(&entry).expect("read entry");
    let _ = crate::bytecode_cache::CacheKey::from_source(&entry, &entry_source);
    let walked = crate::module_source::read(&dependency).expect("dependency was read by the walk");

    runtime.block_on(async {
        let mut vm = super::super::test_runtime::new_vm();
        vm.load_module_exports(&entry)
            .await
            .expect("module load succeeds");

        let canonical = dependency.canonicalize().unwrap_or(dependency.clone());
        let cached = vm
            .source_cache
            .get(&canonical)
            .expect("the loaded dependency is retained for debugger retrieval");
        assert!(
            Arc::ptr_eq(cached, walked.text()),
            "the module loader must bind the bytes the import-graph walk already \
             read instead of reading and copying the file again"
        );
    });
}

#[test]
fn an_edited_dependency_is_re_read_rather_than_served_from_the_shared_owner() {
    // The shared owner is keyed by stat identity, so it must never let a warm
    // process observe stale module bytes. This is the correctness anchor for
    // reusing the entry-key walk's reads.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime builds");
    let temp = tempfile::tempdir().expect("tempdir");
    let entry = temp.path().join("entry.harn");
    let dependency = temp.path().join("value.harn");
    std::fs::write(
        &entry,
        "import { value } from \"./value\"\npub fn read() { return value() }\n",
    )
    .expect("write entry");
    std::fs::write(&dependency, "pub fn value() { return 1 }\n").expect("write dependency");

    runtime.block_on(async {
        let mut first_vm = super::super::test_runtime::new_vm();
        let first = first_vm
            .load_module_exports(&entry)
            .await
            .expect("first module load succeeds");
        let first_result = first_vm
            .call_closure_pub(first.get("read").expect("first export"), &[])
            .await;
        assert!(
            matches!(first_result, Ok(VmValue::Int(1))),
            "unexpected first dependency result: {first_result:?}"
        );

        std::fs::write(&dependency, "pub fn value() { return 2 }\n").expect("rewrite dependency");

        let mut second_vm = super::super::test_runtime::new_vm();
        let second = second_vm
            .load_module_exports(&entry)
            .await
            .expect("second module load succeeds");
        let second_result = second_vm
            .call_closure_pub(second.get("read").expect("second export"), &[])
            .await;
        assert!(
            matches!(second_result, Ok(VmValue::Int(2))),
            "an edited dependency must be re-read in the same process: {second_result:?}"
        );
    });
}

/// A module, its compiled artifact stored where a keyed lookup will find it,
/// and a link table naming it — the state a warm spawn arrives in.
///
/// The artifact goes to the path adjacent to the source rather than the shared
/// cache directory, so the test neither reads nor writes the developer's cache.
fn seed_linked_module(
    temp: &Path,
    body: &str,
) -> (PathBuf, Arc<crate::context_manifest::GraphLinkTable>) {
    let entry = temp.join("entry.harn");
    std::fs::write(&entry, "import \"./dep\"\n").expect("write entry");
    let dep = temp.join("dep.harn");
    std::fs::write(&dep, body).expect("write dep");

    let source = module_source::read(&dep).expect("read dep");
    let artifact = compile_module_artifact_from_source(&dep, source.as_str()).expect("compile dep");
    let key = bytecode_cache::CacheKey::from_module_source(
        &source,
        &crate::module_artifact::ModuleCompilationContext::default(),
        crate::module_artifact::ModuleProvenance::User,
    );
    bytecode_cache::store_module_at(
        &bytecode_cache::adjacent_module_cache_path(&dep).expect("adjacent artifact path"),
        &key,
        &artifact,
    )
    .expect("store dep artifact");

    (dep.clone(), link_table_naming(&entry, &dep))
}

/// The table a validated manifest over `[dep]`, anchored at `entry`, produces.
fn link_table_naming(entry: &Path, dep: &Path) -> Arc<crate::context_manifest::GraphLinkTable> {
    let source = module_source::read(dep).expect("read dep");
    let canonical = module_source::canonical_identity(dep);
    let manifest = crate::context_manifest::ContextManifest {
        files: vec![
            crate::context_manifest::ManifestFile::observe(&canonical, &source)
                .expect("observe dep"),
        ],
        ..crate::context_manifest::ContextManifest::begin(module_source::canonical_identity(entry))
    };
    Arc::new(crate::context_manifest::GraphLinkTable::from_validated(
        &manifest,
    ))
}

fn source_reads() -> u64 {
    module_source::SOURCE_READS.with(std::cell::Cell::get)
}

#[test]
fn a_linked_module_resolves_its_artifact_without_reading_the_source() {
    // The point of the whole link table. Nothing observable separates a module
    // resolved through the table from one resolved by reading its file — the
    // recorded digest describes the bytes on disk, so the artifact is the same
    // either way. Only whether the source was consulted differs, which is why
    // the assertion is a read count rather than anything about the module.
    //
    // The falsifier is the second half: without the table the same import must
    // read. Otherwise "zero reads" and "no module was loaded" would look alike.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime builds");
    let temp = tempfile::tempdir().expect("tempdir");
    let (dep, table) = seed_linked_module(temp.path(), "pub fn value() { return 1 }\n");

    runtime.block_on(async {
        let mut linked = super::super::test_runtime::new_vm();
        linked.set_graph_link_table(Some(table));
        let before = source_reads();
        let exports = linked
            .load_module_exports(&dep)
            .await
            .expect("the linked module loads");
        assert!(
            exports.contains_key("value"),
            "the linked module must expose the same exports a read one would"
        );
        assert_eq!(
            source_reads(),
            before,
            "a module the link table names must resolve without reading its source"
        );

        let mut unlinked = super::super::test_runtime::new_vm();
        let before = source_reads();
        unlinked
            .load_module_exports(&dep)
            .await
            .expect("the unlinked module loads");
        assert!(
            source_reads() > before,
            "without a link table the same import must read the file, or the \
             assertion above would hold for a module that never loaded"
        );
    });
}

#[test]
fn a_link_table_naming_an_evicted_artifact_falls_back_to_reading() {
    // A table names an artifact; whether one is on disk under that name is a
    // separate question. Shared cache directories are pruned between spawns, so
    // this is ordinary rather than exotic, and it must cost a recompile rather
    // than a failure.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime builds");
    let temp = tempfile::tempdir().expect("tempdir");
    // A fixed body can collide with an artifact left in the shared cache by an
    // earlier run. Use a test-local key so evicting both lookup locations is
    // deterministic and cannot remove a real user's artifact.
    let body = format!(
        "// isolated cache fixture: {}\npub fn value() {{ return 7 }}\n",
        temp.path().display()
    );
    let (dep, table) = seed_linked_module(temp.path(), &body);
    let key = bytecode_cache::CacheKey::from_module_source(
        &module_source::ModuleSource::from_text(body),
        &crate::module_artifact::ModuleCompilationContext::default(),
        crate::module_artifact::ModuleProvenance::User,
    );
    for path in [
        bytecode_cache::adjacent_module_cache_path(&dep).unwrap(),
        bytecode_cache::cache_dir()
            .expect("cache dir resolves in tests")
            .join(key.module_filename()),
    ] {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("evict {}: {error}", path.display()),
        }
    }

    runtime.block_on(async {
        let mut vm = super::super::test_runtime::new_vm();
        vm.set_graph_link_table(Some(table));
        let before = source_reads();
        let exports = vm
            .load_module_exports(&dep)
            .await
            .expect("an evicted artifact must fall back, not fail");
        let result = vm
            .call_closure_pub(exports.get("value").expect("value export"), &[])
            .await;
        assert!(
            matches!(result, Ok(VmValue::Int(7))),
            "the fallback must produce the module the source describes: {result:?}"
        );
        assert!(
            source_reads() > before,
            "the fallback path is exactly the read the link table was avoiding"
        );
    });
}

#[test]
fn large_public_type_surface_loads_every_schema() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime builds");
    let mut source = String::new();
    for alias in 0..400 {
        source.push_str(&format!("pub type Contract{alias} = {{"));
        for field in 0..12 {
            source.push_str(&format!("field_{field}: string,"));
        }
        source.push_str("}\n");
    }

    runtime.block_on(async {
        let mut vm = super::super::test_runtime::new_vm();
        let loaded = vm
            .load_module_from_source(PathBuf::from("<test>/large_facade.harn"), &source)
            .await
            .expect("large typed facade loads");

        assert_eq!(loaded.public_type_schemas.len(), 400);
        let mut fields = crate::value::DictMap::default();
        for field in 0..12 {
            fields.insert(
                format!("field_{field}").into(),
                VmValue::String(arcstr::ArcStr::from("value")),
            );
        }
        let value = VmValue::dict(fields);
        for name in ["Contract0", "Contract399"] {
            let schema = loaded
                .public_type_schemas
                .get(name)
                .unwrap_or_else(|| panic!("missing schema {name}"));
            assert!(crate::schema::schema_is_value(&value, schema).expect("schema validates"));
        }
    });
}
