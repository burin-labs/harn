use super::*;

fn request(function: &str) -> CallRequest {
    CallRequest {
        adapter: "mcp".to_string(),
        function: function.to_string(),
        arguments: CallArguments::Named(BTreeMap::new()),
        auth: AuthRequest::default(),
        caller: "prepared-generation-test".to_string(),
        replay_key: None,
        trace_id: None,
        parent_span_id: None,
        metadata: BTreeMap::new(),
        cancel_token: None,
        agent_session_id: None,
        agent_event_sink: None,
        actor_chain: None,
        actor_chain_hop: None,
        progress: None,
        tenant_id: None,
        request_id: None,
        auth_context: None,
        auth_principal: None,
    }
}

#[test]
fn dispatch_receipt_distinguishes_measured_zero_from_unavailable() {
    let unavailable = DispatchCallReceipt::default();
    let measured_zero = DispatchCallReceipt {
        generation_cache_hit: Some(true),
        queue_ms: Some(0),
        execution_ms: Some(0),
    };

    assert_eq!(unavailable.queue_ms, None);
    assert_eq!(unavailable.execution_ms, None);
    assert_eq!(measured_zero.queue_ms, Some(0));
    assert_eq!(measured_zero.execution_ms, Some(0));
    assert_ne!(measured_zero, unavailable);
}

#[tokio::test]
async fn prepared_generation_never_rereads_sources_and_isolates_module_state() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("server.harn");
    let counter = dir.path().join("counter.harn");
    std::fs::write(
        &script,
        r#"
import { increment } from "./counter"

pub fn next() -> int {
  return increment()
}
"#,
    )
    .expect("write server");
    std::fs::write(
        &counter,
        r"
let count = 0

pub fn increment() -> int {
  count = count + 1
  return count
}
",
    )
    .expect("write counter");

    let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("prepare core");
    let receipt = core.generation_receipt();
    assert_eq!(receipt.source_modules, 2);
    assert!(receipt.source_bytes > 0);
    assert_eq!(receipt.worker_count, 1);
    assert_eq!(receipt.cache_entries, 2);

    std::fs::remove_file(&script).expect("remove root after preparation");
    std::fs::remove_file(&counter).expect("remove import after preparation");

    let first = core.dispatch(request("next")).await.expect("first call");
    let second = core.dispatch(request("next")).await.expect("second call");
    assert_eq!(first.value, serde_json::json!(1));
    assert_eq!(second.value, serde_json::json!(1));
    assert_eq!(first.dispatch.queue_ms, None);
    assert!(first.dispatch.execution_ms.is_some());
}

#[test]
fn preparation_rejects_an_invalid_import_before_serving() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("server.harn");
    let broken = dir.path().join("broken.harn");
    std::fs::write(
        &script,
        "import { value } from \"./broken\"\npub fn read() { return value() }\n",
    )
    .expect("write server");
    std::fs::write(&broken, "pub fn value( {\n").expect("write invalid import");

    let error = DispatchCore::new(DispatchCoreConfig::for_script(&script))
        .err()
        .expect("invalid generation must fail preparation");
    assert!(error.message().contains("broken.harn"), "{error:?}");
}

#[test]
fn generation_digest_changes_with_exact_source_bytes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("server.harn");
    std::fs::write(&script, "pub fn value() { return 1 }\n").expect("write first source");
    let first = DispatchCore::new(DispatchCoreConfig::for_script(&script))
        .expect("first core")
        .generation_receipt();
    std::fs::write(&script, "pub fn value() { return 2 }\n").expect("write second source");
    let second = DispatchCore::new(DispatchCoreConfig::for_script(&script))
        .expect("second core")
        .generation_receipt();

    assert_ne!(first.source_digest_blake3, second.source_digest_blake3);
}

#[test]
fn generation_digest_is_stable_across_checkout_roots() {
    fn prepare(root: &std::path::Path) -> DispatchGenerationReceipt {
        let source = root.join("src");
        std::fs::create_dir_all(&source).expect("create source tree");
        let script = source.join("server.harn");
        let dependency = source.join("dependency.harn");
        std::fs::write(
            &script,
            "import { value } from \"./dependency\"\npub fn read() { return value() }\n",
        )
        .expect("write root");
        std::fs::write(&dependency, "pub fn value() { return 7 }\n").expect("write dependency");
        DispatchCore::new(DispatchCoreConfig::for_script(&script))
            .expect("prepare graph")
            .generation_receipt()
    }

    let first_root = tempfile::tempdir().expect("first checkout");
    let second_root = tempfile::tempdir().expect("second checkout");
    let first = prepare(first_root.path());
    let second = prepare(second_root.path());

    assert_eq!(first.source_digest_blake3, second.source_digest_blake3);
    assert_eq!(first.source_modules, second.source_modules);
    assert_eq!(first.source_bytes, second.source_bytes);
}

#[test]
fn unknown_or_partially_annotated_exports_remain_exclusive() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("server.harn");
    std::fs::write(
        &script,
        r"
pub fn unknown() -> nil { return nil }

@annotations(readOnly: true)
pub fn read_only_only() -> nil { return nil }

@annotations(readOnly: true, idempotent: true)
pub fn explicit_safe() -> nil { return nil }
",
    )
    .expect("write script");
    let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");

    assert!(!core.is_concurrent_dispatch("unknown"));
    assert!(!core.is_concurrent_dispatch("read_only_only"));
    assert!(core.is_concurrent_dispatch("explicit_safe"));
    assert!(!core.is_concurrent_dispatch("missing"));
}
