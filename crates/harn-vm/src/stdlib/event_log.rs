use std::collections::BTreeMap;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use futures::StreamExt;

use crate::event_log::{
    active_event_log, install_memory_for_current_thread, EventLog, LogEvent, Topic,
};
use crate::llm::vm_value_to_json;
use crate::runtime_limits::RuntimeLimits;
use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{VmError, VmResourceHandle, VmStream, VmValue};
use crate::vm::Vm;

mod hypothesis_authority;
mod hypothesis_host_attestation;
#[cfg(test)]
mod hypothesis_persistence_tests;

pub use hypothesis_authority::mint_hypothesis_native_attestation;
use hypothesis_authority::{
    insert_hypothesis_authority_headers, normalize_hypothesis_projection_headers,
    proof_from_native_attestation, verify_hypothesis_append_outcome, HypothesisAuthorityKind,
    HypothesisEventAuthorityProof,
};

const EVENT_LOG_QUEUE_DEPTH: usize = RuntimeLimits::DEFAULT.default_event_log_queue_depth;
const IDEMPOTENCY_HEADER: &str = "harn.idempotency_key";
const HYPOTHESIS_LEDGER_TOPIC: &str = "hypotheses.events.v1";
const HYPOTHESIS_AUTHORITY_HANDLE: &str = "hypothesis_event_authority";
const HYPOTHESIS_NATIVE_ATTESTATION_HANDLE: &str = "hypothesis_native_attestation";
const HYPOTHESIS_AUTHORITY_SCHEMA: &str = "harn.hypothesis-event-authority.v1";
const HYPOTHESIS_AUTHORITY_SCHEMA_HEADER: &str = "harn.hypothesis.authority.schema";
const HYPOTHESIS_AUTHORITY_KIND_HEADER: &str = "harn.hypothesis.authority.kind";
const HYPOTHESIS_EVENT_FINGERPRINT_HEADER: &str = "harn.hypothesis.event_fingerprint";
const HYPOTHESIS_PLAN_FINGERPRINT_HEADER: &str = "harn.hypothesis.plan_fingerprint";
const HYPOTHESIS_ID_HEADER: &str = "harn.hypothesis.hypothesis_id";
const HYPOTHESIS_RUN_ID_HEADER: &str = "harn.hypothesis.run_id";

const HYPOTHESIS_AUTHORITY_HEADERS: &[&str] = &[
    HYPOTHESIS_AUTHORITY_SCHEMA_HEADER,
    HYPOTHESIS_AUTHORITY_KIND_HEADER,
    HYPOTHESIS_EVENT_FINGERPRINT_HEADER,
    HYPOTHESIS_PLAN_FINGERPRINT_HEADER,
    HYPOTHESIS_ID_HEADER,
    HYPOTHESIS_RUN_ID_HEADER,
];

pub(crate) fn register_event_log_builtins(vm: &mut Vm) {
    register_event_log_namespace(vm);
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

#[harn_builtin(
    exposure = "harness.obs.event_log_describe",
    effects = ["state.read@dynamic"],
    sig = "event_log.describe() -> dict",
    kind = "async",
    category = "event_log"
)]
async fn event_log_describe_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    if !args.is_empty() {
        return Err(VmError::TypeError(
            "event_log.describe: expected no arguments".to_string(),
        ));
    }
    let description = ensure_event_log().describe();
    Ok(crate::stdlib::json_to_vm_value(&serde_json::json!({
        "backend": description.backend.to_string(),
        "location": description
            .location
            .map(|path| path.to_string_lossy().into_owned()),
        "size_bytes": description.size_bytes,
        "queue_depth": description.queue_depth,
    })))
}

#[harn_builtin(
    exposure = "harness.obs.event_log_emit",
    effects = ["observability.write@arg0"],
    sig = "event_log.emit(topic: string, kind: string, payload?: any, headers?: dict) -> int",
    kind = "async",
    category = "event_log"
)]
async fn event_log_emit_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let topic = parse_topic(args.first(), "event_log.emit")?;
    reject_reserved_public_emit_topic(&topic, "event_log.emit")?;
    let kind = required_string(args.get(1), "event_log.emit", "kind")?;
    let payload = args
        .get(2)
        .map(vm_value_to_json)
        .unwrap_or(serde_json::Value::Null);
    let headers = parse_headers(args.get(3), "event_log.emit")?;
    let id = ensure_event_log()
        .append(&topic, LogEvent::new(kind, payload).with_headers(headers))
        .await
        .map_err(log_error)?;
    Ok(VmValue::Int(id as i64))
}

#[harn_builtin(
    exposure = "harness.obs.event_log_emit_idempotent",
    effects = ["observability.write@arg0"],
    sig = "event_log.emit_idempotent(topic: string, kind: string, idempotency_key: string, payload?: any, headers?: dict) -> dict",
    kind = "async",
    category = "event_log"
)]
async fn event_log_emit_idempotent_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let topic = parse_topic(args.first(), "event_log.emit_idempotent")?;
    reject_reserved_public_emit_topic(&topic, "event_log.emit_idempotent")?;
    let kind = required_string(args.get(1), "event_log.emit_idempotent", "kind")?;
    let idempotency_key =
        required_string(args.get(2), "event_log.emit_idempotent", "idempotency_key")?;
    if idempotency_key.trim().is_empty() {
        return Err(VmError::TypeError(
            "event_log.emit_idempotent: idempotency_key cannot be empty".to_string(),
        ));
    }
    let payload = args
        .get(3)
        .map(vm_value_to_json)
        .unwrap_or(serde_json::Value::Null);
    let mut headers = parse_headers(args.get(4), "event_log.emit_idempotent")?;
    if headers.contains_key(IDEMPOTENCY_HEADER) {
        return Err(VmError::TypeError(format!(
            "event_log.emit_idempotent: header '{IDEMPOTENCY_HEADER}' is reserved"
        )));
    }
    headers.insert(IDEMPOTENCY_HEADER.to_string(), idempotency_key.clone());
    let topic_name = topic.as_str().to_string();
    let outcome = ensure_event_log()
        .append_idempotent_by_header(
            &topic,
            IDEMPOTENCY_HEADER,
            &idempotency_key,
            LogEvent::new(kind, payload).with_headers(headers),
        )
        .await
        .map_err(log_error)?;
    Ok(append_outcome_to_value(&topic_name, outcome))
}

#[harn_builtin(
    exposure = "harness.obs.event_log_emit_idempotent_chained",
    effects = ["observability.write@arg0"],
    sig = "event_log.emit_idempotent_chained(topic: string, kind: string, idempotency_key: string, expected_head: string?, payload?: any, headers?: dict) -> dict",
    kind = "async",
    category = "event_log"
)]
async fn event_log_emit_idempotent_chained_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let builtin = "event_log.emit_idempotent_chained";
    let topic = parse_topic(args.first(), builtin)?;
    reject_reserved_public_emit_topic(&topic, builtin)?;
    let kind = required_string(args.get(1), builtin, "kind")?;
    let idempotency_key = required_string(args.get(2), builtin, "idempotency_key")?;
    if idempotency_key.trim().is_empty() {
        return Err(VmError::TypeError(format!(
            "{builtin}: idempotency_key cannot be empty"
        )));
    }
    let expected_head = optional_string(args.get(3), builtin, "expected_head")?;
    if expected_head
        .as_deref()
        .is_some_and(|head| head.trim().is_empty())
    {
        return Err(VmError::TypeError(format!(
            "{builtin}: expected_head cannot be empty; use nil for an empty topic"
        )));
    }
    let payload = args
        .get(4)
        .map(vm_value_to_json)
        .unwrap_or(serde_json::Value::Null);
    let mut headers = parse_headers(args.get(5), builtin)?;
    if headers.contains_key(IDEMPOTENCY_HEADER) {
        return Err(VmError::TypeError(format!(
            "{builtin}: header '{IDEMPOTENCY_HEADER}' is reserved"
        )));
    }
    headers.insert(IDEMPOTENCY_HEADER.to_string(), idempotency_key.clone());
    let topic_name = topic.as_str().to_string();
    let outcome = ensure_event_log()
        .append_idempotent_chained_by_header(
            &topic,
            IDEMPOTENCY_HEADER,
            &idempotency_key,
            expected_head.as_deref(),
            LogEvent::new(kind, payload).with_headers(headers),
        )
        .await
        .map_err(log_error)?;
    Ok(append_outcome_to_value(&topic_name, outcome))
}

#[harn_builtin(
    exposure = "harness.obs.hypothesis_event_authority_mint",
    effects = ["authority.write@arg1"],
    sig = "event_log.hypothesis_authority_mint(native_attestation: resource, authority_kind: string, event_fingerprint: string, plan_fingerprint: string, hypothesis_id: string, run_id?: string) -> resource",
    kind = "async",
    category = "event_log"
)]
async fn hypothesis_event_authority_mint_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let builtin = "event_log.hypothesis_authority_mint";
    let attested = proof_from_native_attestation(args.first(), builtin)?;
    let authority_kind = HypothesisAuthorityKind::parse(
        &required_non_empty_string(args.get(1), builtin, "authority_kind")?,
        builtin,
    )?;
    let event_fingerprint = required_sha256_fingerprint(args.get(2), builtin, "event_fingerprint")?;
    let plan_fingerprint = required_sha256_fingerprint(args.get(3), builtin, "plan_fingerprint")?;
    let hypothesis_id = required_non_empty_string(args.get(4), builtin, "hypothesis_id")?;
    let run_id = optional_non_empty_string(args.get(5), builtin, "run_id")?;
    if attested.authority_kind != authority_kind
        || attested.event_fingerprint.as_ref() != event_fingerprint
        || attested.plan_fingerprint.as_ref() != plan_fingerprint
        || attested.hypothesis_id.as_ref() != hypothesis_id
        || attested.run_id.as_deref() != run_id.as_deref()
        || attested.execution_scope
            != crate::observability::execution_scope::current_execution_scope()
    {
        return Err(VmError::Runtime(format!(
            "{builtin}: arguments do not match the registered native adapter attestation"
        )));
    }
    let proof = HypothesisEventAuthorityProof {
        authority_kind,
        event_fingerprint: Arc::from(event_fingerprint),
        plan_fingerprint: Arc::from(plan_fingerprint),
        hypothesis_id: Arc::from(hypothesis_id),
        run_id: run_id.map(Arc::from),
        execution_scope: crate::observability::execution_scope::current_execution_scope(),
    };
    Ok(VmValue::resource(VmResourceHandle::new(
        HYPOTHESIS_AUTHORITY_HANDLE,
        proof,
    )))
}

#[harn_builtin(
    exposure = "harness.obs.hypothesis_event_append",
    effects = ["observability.write@const=hypotheses.events.v1"],
    sig = "event_log.hypothesis_event_append(proof: resource, kind: string, idempotency_key: string, expected_head: string?, event_fingerprint: string, plan_fingerprint: string, hypothesis_id: string, run_id: string?, payload: any, headers?: dict) -> dict",
    kind = "async",
    category = "event_log"
)]
async fn hypothesis_event_append_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let builtin = "event_log.hypothesis_event_append";
    let proof = hypothesis_authority_proof(args.first(), builtin)?;
    let kind = required_non_empty_string(args.get(1), builtin, "kind")?;
    let idempotency_key = required_non_empty_string(args.get(2), builtin, "idempotency_key")?;
    let expected_head = optional_non_empty_string(args.get(3), builtin, "expected_head")?;
    let event_fingerprint = required_sha256_fingerprint(args.get(4), builtin, "event_fingerprint")?;
    let plan_fingerprint = required_sha256_fingerprint(args.get(5), builtin, "plan_fingerprint")?;
    let hypothesis_id = required_non_empty_string(args.get(6), builtin, "hypothesis_id")?;
    let run_id = optional_non_empty_string(args.get(7), builtin, "run_id")?;
    verify_hypothesis_authority_binding(
        &proof,
        &event_fingerprint,
        &plan_fingerprint,
        &hypothesis_id,
        run_id.as_deref(),
        builtin,
    )?;
    if idempotency_key != event_fingerprint {
        return Err(VmError::Runtime(format!(
            "{builtin}: idempotency key must equal the authorized event fingerprint"
        )));
    }

    let payload_value = args
        .get(8)
        .ok_or_else(|| VmError::TypeError(format!("{builtin}: missing payload")))?;
    verify_hypothesis_event_payload(payload_value, &event_fingerprint, builtin)?;
    let payload = vm_value_to_json(payload_value);
    let mut headers = parse_headers(args.get(9), builtin)?;
    reject_reserved_headers(&headers, builtin)?;
    normalize_hypothesis_projection_headers(&mut headers, payload_value, &proof, &kind, builtin)?;
    if headers.contains_key(IDEMPOTENCY_HEADER) {
        return Err(VmError::TypeError(format!(
            "{builtin}: header '{IDEMPOTENCY_HEADER}' is reserved"
        )));
    }
    headers.insert(IDEMPOTENCY_HEADER.to_string(), idempotency_key.clone());
    insert_hypothesis_authority_headers(&mut headers, &proof);
    let expected_headers = headers.clone();

    let topic = Topic::new(HYPOTHESIS_LEDGER_TOPIC).expect("static hypothesis topic is valid");
    let outcome = ensure_event_log()
        .append_idempotent_chained_by_header(
            &topic,
            IDEMPOTENCY_HEADER,
            &idempotency_key,
            expected_head.as_deref(),
            LogEvent::new(kind.clone(), payload.clone()).with_headers(headers),
        )
        .await
        .map_err(log_error)?;
    verify_hypothesis_append_outcome(
        &outcome.event,
        &proof,
        &kind,
        &payload,
        &expected_headers,
        builtin,
    )?;
    Ok(append_outcome_to_value(HYPOTHESIS_LEDGER_TOPIC, outcome))
}

#[harn_builtin(
    exposure = "harness.obs.event_log_latest",
    effects = ["state.read@arg0"],
    sig = "event_log.latest(topic: string) -> int | nil",
    kind = "async",
    category = "event_log"
)]
async fn event_log_latest_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let topic = parse_topic(args.first(), "event_log.latest")?;
    let latest = ensure_event_log().latest(&topic).await.map_err(log_error)?;
    Ok(latest
        .map(|id| VmValue::Int(id as i64))
        .unwrap_or(VmValue::Nil))
}

#[harn_builtin(
    exposure = "harness.obs.event_log_verify",
    effects = ["state.read@arg0"],
    sig = "event_log.verify(topic: string) -> {verified: bool, errors: list<string>, event_count: int, last_hash: string?}",
    kind = "async",
    category = "event_log"
)]
async fn event_log_verify_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let topic = parse_topic(args.first(), "event_log.verify")?;
    let report = ensure_event_log()
        .verify_topic(&topic)
        .await
        .map_err(log_error)?;
    Ok(crate::stdlib::json_to_vm_value(&serde_json::json!({
        "verified": report.verified,
        "errors": report.errors,
        "event_count": report.event_count,
        "last_hash": report.last_hash,
    })))
}

#[harn_builtin(
    exposure = "harness.obs.event_log_read",
    effects = ["state.read@arg0"],
    sig = "event_log.read(topic_or_options: string | dict, from_cursor?: int | nil, limit?: int) -> list",
    kind = "async",
    category = "event_log"
)]
async fn event_log_read_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let options = parse_read_options(&args)?;
    let events = ensure_event_log()
        .read_range(&options.topic, options.from_cursor, options.limit)
        .await
        .map_err(log_error)?;
    let topic_name = options.topic.as_str().to_string();
    Ok(VmValue::List(std::sync::Arc::new(
        events
            .into_iter()
            .filter(|(_, event)| {
                options
                    .kind_prefix
                    .as_deref()
                    .is_none_or(|prefix| event.kind.starts_with(prefix))
            })
            .map(|(event_id, event)| event_to_value(&topic_name, event_id, event))
            .collect(),
    )))
}

#[harn_builtin(
    exposure = "harness.obs.hypothesis_event_snapshot",
    effects = ["state.read@const=hypotheses.events.v1"],
    sig = "event_log.hypothesis_event_snapshot(hypothesis_id: string) -> {verified: bool, errors: list<string>, scanned_event_count: int, last_hash: string?, records: list}",
    kind = "async",
    category = "event_log"
)]
async fn hypothesis_event_snapshot_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let builtin = "event_log.hypothesis_event_snapshot";
    let hypothesis_id = required_non_empty_string(args.first(), builtin, "hypothesis_id")?;
    let topic = Topic::new(HYPOTHESIS_LEDGER_TOPIC).expect("static hypothesis topic is valid");
    let snapshot = ensure_event_log()
        .verified_snapshot_by_header(&topic, "hypothesis_id", &hypothesis_id)
        .await
        .map_err(log_error)?;
    let topic_name = topic.as_str().to_string();
    let records: Vec<VmValue> = snapshot
        .events
        .into_iter()
        .map(|(event_id, event)| event_to_value(&topic_name, event_id, event))
        .collect();
    Ok(crate::stdlib::json_to_vm_value(&serde_json::json!({
        "verified": snapshot.verification.verified,
        "errors": snapshot.verification.errors,
        "scanned_event_count": snapshot.verification.event_count,
        "last_hash": snapshot.verification.last_hash,
        "records": records.into_iter().map(|record| vm_value_to_json(&record)).collect::<Vec<_>>(),
    })))
}

#[harn_builtin(
    exposure = "harness.obs.event_log_subscribe",
    effects = ["state.observe@arg0"],
    sig = "event_log.subscribe(topic_or_options: string | dict, from_cursor?: int | nil) -> stream",
    kind = "async",
    category = "event_log"
)]
async fn event_log_subscribe_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let options = parse_subscribe_options(&args)?;
    let log = ensure_event_log();
    let mut events = log
        .clone()
        .subscribe(&options.topic, options.from_cursor)
        .await
        .map_err(log_error)?;
    let topic_name = options.topic.as_str().to_string();
    let kind_prefix = options.kind_prefix.clone();
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<VmValue, VmError>>(1);

    tokio::task::spawn_local(async move {
        while let Some(next) = events.next().await {
            let value = match next {
                Ok((event_id, event)) => {
                    if kind_prefix
                        .as_deref()
                        .is_some_and(|prefix| !event.kind.starts_with(prefix))
                    {
                        continue;
                    }
                    Ok(event_to_value(&topic_name, event_id, event))
                }
                Err(error) => Err(log_error(error)),
            };
            if tx.send(value).await.is_err() {
                return;
            }
        }
    });

    Ok(VmValue::stream(VmStream {
        done: Arc::new(AtomicBool::new(false)),
        receiver: Arc::new(tokio::sync::Mutex::new(rx)),
        cancel: None,
    }))
}

#[harn_builtin(
    exposure = "harness.obs.event_log_topics",
    effects = ["state.read@const=event-log"],
    sig = "event_log.topics() -> list<string>",
    kind = "async",
    category = "event_log"
)]
async fn event_log_topics_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    if !args.is_empty() {
        return Err(VmError::TypeError(
            "event_log.topics: expected no arguments".to_string(),
        ));
    }
    Ok(VmValue::List(std::sync::Arc::new(
        ensure_event_log()
            .topics()
            .await
            .map_err(log_error)?
            .into_iter()
            .map(|topic| VmValue::String(arcstr::ArcStr::from(topic.as_str())))
            .collect(),
    )))
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    &EVENT_LOG_DESCRIBE_IMPL_DEF,
    &EVENT_LOG_EMIT_IMPL_DEF,
    &EVENT_LOG_EMIT_IDEMPOTENT_IMPL_DEF,
    &EVENT_LOG_EMIT_IDEMPOTENT_CHAINED_IMPL_DEF,
    &HYPOTHESIS_EVENT_AUTHORITY_MINT_IMPL_DEF,
    &hypothesis_host_attestation::HYPOTHESIS_EVENT_AUTHORITY_REQUEST_IMPL_DEF,
    &hypothesis_host_attestation::HYPOTHESIS_OPERATION_REQUEST_IMPL_DEF,
    &HYPOTHESIS_EVENT_APPEND_IMPL_DEF,
    &HYPOTHESIS_EVENT_SNAPSHOT_IMPL_DEF,
    &EVENT_LOG_LATEST_IMPL_DEF,
    &EVENT_LOG_READ_IMPL_DEF,
    &EVENT_LOG_SUBSCRIBE_IMPL_DEF,
    &EVENT_LOG_TOPICS_IMPL_DEF,
    &EVENT_LOG_VERIFY_IMPL_DEF,
];

fn register_event_log_namespace(vm: &mut Vm) {
    let names = [
        "describe",
        "emit",
        "emit_idempotent",
        "emit_idempotent_chained",
        "hypothesis_authority_mint",
        "hypothesis_authority_request",
        "hypothesis_operation_request",
        "hypothesis_event_append",
        "hypothesis_event_snapshot",
        "latest",
        "read",
        "subscribe",
        "topics",
        "verify",
    ];
    vm.set_global(
        "event_log",
        VmValue::dict(
            std::iter::once((
                "_namespace".to_string(),
                VmValue::String(arcstr::ArcStr::from("event_log")),
            ))
            .chain(names.into_iter().map(|name| {
                (
                    name.to_string(),
                    VmValue::BuiltinRef(arcstr::ArcStr::from(format!("event_log.{name}"))),
                )
            }))
            .collect::<BTreeMap<_, _>>(),
        ),
    );
}

struct SubscribeOptions {
    topic: Topic,
    from_cursor: Option<u64>,
    kind_prefix: Option<String>,
}

struct ReadOptions {
    topic: Topic,
    from_cursor: Option<u64>,
    limit: usize,
    kind_prefix: Option<String>,
}

const EVENT_LOG_READ_DEFAULT_LIMIT: usize = 100;
const EVENT_LOG_READ_MAX_LIMIT: usize = 10_000;

fn parse_read_options(args: &[VmValue]) -> Result<ReadOptions, VmError> {
    match args.first() {
        Some(VmValue::Dict(options)) => {
            let topic = parse_topic(options.get("topic"), "event_log.read")?;
            let from_cursor = parse_dict_cursor(options, "event_log.read")?;
            let limit = parse_limit(options.get("limit"), "event_log.read")?;
            let kind_prefix =
                optional_string(options.get("kind_prefix"), "event_log.read", "kind_prefix")?;
            Ok(ReadOptions {
                topic,
                from_cursor,
                limit,
                kind_prefix,
            })
        }
        other => Ok(ReadOptions {
            topic: parse_topic(other, "event_log.read")?,
            from_cursor: parse_cursor(args.get(1), "event_log.read")?,
            limit: parse_limit(args.get(2), "event_log.read")?,
            kind_prefix: None,
        }),
    }
}

fn parse_subscribe_options(args: &[VmValue]) -> Result<SubscribeOptions, VmError> {
    match args.first() {
        Some(VmValue::Dict(options)) => {
            let topic = parse_topic(options.get("topic"), "event_log.subscribe")?;
            let from_cursor = parse_dict_cursor(options, "event_log.subscribe")?;
            let kind_prefix = optional_string(
                options.get("kind_prefix"),
                "event_log.subscribe",
                "kind_prefix",
            )?;
            Ok(SubscribeOptions {
                topic,
                from_cursor,
                kind_prefix,
            })
        }
        other => Ok(SubscribeOptions {
            topic: parse_topic(other, "event_log.subscribe")?,
            from_cursor: parse_cursor(args.get(1), "event_log.subscribe")?,
            kind_prefix: None,
        }),
    }
}

fn ensure_event_log() -> std::sync::Arc<crate::event_log::AnyEventLog> {
    if let Some(ctx) = crate::connectors::harn_module::active_harn_connector_ctx() {
        return ctx.event_log;
    }
    active_event_log().unwrap_or_else(|| install_memory_for_current_thread(EVENT_LOG_QUEUE_DEPTH))
}

fn reject_reserved_public_emit_topic(topic: &Topic, builtin: &str) -> Result<(), VmError> {
    if topic.as_str() == HYPOTHESIS_LEDGER_TOPIC {
        return Err(VmError::Runtime(format!(
            "{builtin}: topic '{HYPOTHESIS_LEDGER_TOPIC}' is reserved for native hypothesis authority"
        )));
    }
    Ok(())
}

fn hypothesis_authority_proof(
    value: Option<&VmValue>,
    builtin: &str,
) -> Result<Arc<HypothesisEventAuthorityProof>, VmError> {
    let Some(VmValue::Resource(handle)) = value else {
        return Err(VmError::TypeError(format!(
            "{builtin}: proof must be a native hypothesis event authority resource"
        )));
    };
    if handle.label() != HYPOTHESIS_AUTHORITY_HANDLE {
        return Err(VmError::TypeError(format!(
            "{builtin}: proof must be a native hypothesis event authority resource"
        )));
    }
    handle
        .downcast::<HypothesisEventAuthorityProof>()
        .ok_or_else(|| {
            VmError::Runtime(format!(
                "{builtin}: invalid hypothesis event authority resource"
            ))
        })
}

fn verify_hypothesis_authority_binding(
    proof: &HypothesisEventAuthorityProof,
    event_fingerprint: &str,
    plan_fingerprint: &str,
    hypothesis_id: &str,
    run_id: Option<&str>,
    builtin: &str,
) -> Result<(), VmError> {
    if proof.event_fingerprint.as_ref() != event_fingerprint {
        return Err(VmError::Runtime(format!(
            "{builtin}: event fingerprint does not match native authority"
        )));
    }
    if proof.plan_fingerprint.as_ref() != plan_fingerprint {
        return Err(VmError::Runtime(format!(
            "{builtin}: plan fingerprint does not match native authority"
        )));
    }
    if proof.hypothesis_id.as_ref() != hypothesis_id {
        return Err(VmError::Runtime(format!(
            "{builtin}: hypothesis id does not match native authority"
        )));
    }
    if proof.run_id.as_deref() != run_id {
        return Err(VmError::Runtime(format!(
            "{builtin}: run id does not match native authority"
        )));
    }
    if proof.execution_scope != crate::observability::execution_scope::current_execution_scope() {
        return Err(VmError::Runtime(format!(
            "{builtin}: native authority belongs to a different execution scope"
        )));
    }
    Ok(())
}

fn hypothesis_event_field<'a>(value: &'a VmValue, field: &str) -> Option<&'a VmValue> {
    match value {
        VmValue::Dict(fields) => fields.get(field),
        VmValue::StructInstance(_) => value.struct_field(field),
        _ => None,
    }
}

fn verify_hypothesis_event_payload(
    payload: &VmValue,
    event_fingerprint: &str,
    builtin: &str,
) -> Result<(), VmError> {
    let content = hypothesis_event_field(payload, "content").ok_or_else(|| {
        VmError::TypeError(format!(
            "{builtin}: payload must contain hypothesis event content"
        ))
    })?;
    let declared = match hypothesis_event_field(payload, "fingerprint") {
        Some(VmValue::String(value)) => value.as_str(),
        _ => {
            return Err(VmError::TypeError(format!(
                "{builtin}: payload must contain a string fingerprint"
            )))
        }
    };
    let canonical = crate::stdlib::json::vm_value_to_json(content);
    let computed = format!(
        "sha256:{}",
        harn_kernel::pure::sha256_hex(canonical.as_bytes())
    );
    if declared != event_fingerprint || computed != event_fingerprint {
        return Err(VmError::Runtime(format!(
            "{builtin}: payload content does not match the authorized event fingerprint"
        )));
    }
    Ok(())
}

fn reject_reserved_headers(
    headers: &BTreeMap<String, String>,
    builtin: &str,
) -> Result<(), VmError> {
    if let Some(header) = headers.keys().find(|header| {
        HYPOTHESIS_AUTHORITY_HEADERS.contains(&header.as_str())
            || header.starts_with("harn.provenance.")
    }) {
        return Err(VmError::TypeError(format!(
            "{builtin}: header '{header}' is reserved"
        )));
    }
    Ok(())
}

fn parse_topic(value: Option<&VmValue>, builtin: &str) -> Result<Topic, VmError> {
    let raw = required_string(value, builtin, "topic")?;
    Topic::new(raw).map_err(log_error)
}

fn parse_cursor(value: Option<&VmValue>, builtin: &str) -> Result<Option<u64>, VmError> {
    match value {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::Int(n)) if *n >= 0 => Ok(Some(*n as u64)),
        Some(other) => Err(VmError::TypeError(format!(
            "{builtin}: from_cursor must be a non-negative int or nil, got {}",
            other.type_name()
        ))),
    }
}

/// Read the cursor from an options dict, accepting `from_cursor`, `cursor`, and
/// `from` as aliases. Shared by the `read` and `subscribe` dict forms.
fn parse_dict_cursor(
    options: &crate::value::DictMap,
    builtin: &str,
) -> Result<Option<u64>, VmError> {
    parse_cursor(
        options
            .get("from_cursor")
            .or_else(|| options.get("cursor"))
            .or_else(|| options.get("from")),
        builtin,
    )
}

fn parse_limit(value: Option<&VmValue>, builtin: &str) -> Result<usize, VmError> {
    match value {
        None | Some(VmValue::Nil) => Ok(EVENT_LOG_READ_DEFAULT_LIMIT),
        Some(VmValue::Int(n)) if *n >= 0 => Ok((*n as usize).min(EVENT_LOG_READ_MAX_LIMIT)),
        Some(other) => Err(VmError::TypeError(format!(
            "{builtin}: limit must be a non-negative int or nil, got {}",
            other.type_name()
        ))),
    }
}

fn required_string(value: Option<&VmValue>, builtin: &str, name: &str) -> Result<String, VmError> {
    match value {
        Some(VmValue::String(value)) => Ok(value.to_string()),
        Some(other) => Err(VmError::TypeError(format!(
            "{builtin}: {name} must be a string, got {}",
            other.type_name()
        ))),
        None => Err(VmError::TypeError(format!("{builtin}: missing {name}"))),
    }
}

fn required_non_empty_string(
    value: Option<&VmValue>,
    builtin: &str,
    name: &str,
) -> Result<String, VmError> {
    let value = required_string(value, builtin, name)?;
    if value.trim().is_empty() {
        return Err(VmError::TypeError(format!(
            "{builtin}: {name} cannot be empty"
        )));
    }
    Ok(value)
}

fn required_sha256_fingerprint(
    value: Option<&VmValue>,
    builtin: &str,
    name: &str,
) -> Result<String, VmError> {
    let value = required_non_empty_string(value, builtin, name)?;
    let Some(digest) = value.strip_prefix("sha256:") else {
        return Err(VmError::TypeError(format!(
            "{builtin}: {name} must use sha256:<64 lowercase hex>"
        )));
    };
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(VmError::TypeError(format!(
            "{builtin}: {name} must use sha256:<64 lowercase hex>"
        )));
    }
    Ok(value)
}

fn optional_non_empty_string(
    value: Option<&VmValue>,
    builtin: &str,
    name: &str,
) -> Result<Option<String>, VmError> {
    let value = optional_string(value, builtin, name)?;
    if value
        .as_deref()
        .is_some_and(|value| value.trim().is_empty())
    {
        return Err(VmError::TypeError(format!(
            "{builtin}: {name} cannot be empty; use nil when absent"
        )));
    }
    Ok(value)
}

fn optional_string(
    value: Option<&VmValue>,
    builtin: &str,
    name: &str,
) -> Result<Option<String>, VmError> {
    match value {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::String(value)) => Ok(Some(value.to_string())),
        Some(other) => Err(VmError::TypeError(format!(
            "{builtin}: {name} must be a string or nil, got {}",
            other.type_name()
        ))),
    }
}

fn parse_headers(
    value: Option<&VmValue>,
    builtin: &str,
) -> Result<BTreeMap<String, String>, VmError> {
    match value {
        None | Some(VmValue::Nil) => Ok(BTreeMap::new()),
        Some(VmValue::Dict(dict)) => {
            let mut out = BTreeMap::new();
            for (key, value) in dict.iter() {
                match value {
                    VmValue::String(value) => {
                        out.insert(key.to_string(), value.to_string());
                    }
                    other => {
                        return Err(VmError::TypeError(format!(
                            "{builtin}: header '{key}' must be a string, got {}",
                            other.type_name()
                        )))
                    }
                }
            }
            Ok(out)
        }
        Some(other) => Err(VmError::TypeError(format!(
            "{builtin}: headers must be a dict, got {}",
            other.type_name()
        ))),
    }
}

fn event_to_value(topic: &str, event_id: u64, event: LogEvent) -> VmValue {
    crate::stdlib::json_to_vm_value(&serde_json::json!({
        "id": event_id,
        "cursor": event_id,
        "topic": topic,
        "kind": event.kind,
        "payload": event.payload,
        "headers": event.headers,
        "occurred_at_ms": event.occurred_at_ms,
    }))
}

fn append_outcome_to_value(topic: &str, outcome: crate::event_log::AppendOutcome) -> VmValue {
    crate::stdlib::json_to_vm_value(&serde_json::json!({
        "event_id": outcome.event_id,
        "inserted": outcome.inserted,
        "event": vm_value_to_json(&event_to_value(topic, outcome.event_id, outcome.event)),
    }))
}

fn log_error(error: crate::event_log::LogError) -> VmError {
    VmError::Runtime(format!("event_log: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_log::reset_active_event_log;
    use crate::observability::execution_scope::{enter_execution_scope, mint_execution_scope};
    use crate::orchestration::{pop_execution_policy, push_execution_policy, CapabilityPolicy};

    fn string(value: &str) -> VmValue {
        VmValue::String(arcstr::ArcStr::from(value))
    }

    fn dict(entries: impl IntoIterator<Item = (&'static str, VmValue)>) -> VmValue {
        VmValue::dict(
            entries
                .into_iter()
                .map(|(key, value)| (crate::value::intern_key(key), value))
                .collect::<crate::value::DictMap>(),
        )
    }

    fn plan_fingerprint() -> String {
        format!("sha256:{}", "b".repeat(64))
    }

    fn hypothesis_event_payload_from_content(content: VmValue) -> (VmValue, String) {
        let canonical = crate::stdlib::json::vm_value_to_json(&content);
        let fingerprint = format!(
            "sha256:{}",
            harn_kernel::pure::sha256_hex(canonical.as_bytes())
        );
        let payload = dict([("content", content), ("fingerprint", string(&fingerprint))]);
        (payload, fingerprint)
    }

    fn hypothesis_event_payload() -> (VmValue, String) {
        hypothesis_event_payload_from_content(dict([
            ("schema", string("harn.hypothesis.event.v1")),
            ("event_id", string("event-1")),
            ("hypothesis_id", string("hyp-1")),
            ("plan_id", VmValue::Nil),
            ("run_id", VmValue::Nil),
            ("payload", dict([("kind", string("plan_registered"))])),
        ]))
    }

    fn ctx() -> crate::vm::AsyncBuiltinCtx {
        crate::vm::AsyncBuiltinCtx::for_test(Vm::new())
    }

    fn mint_policy_args(authority_kind: &str) -> Vec<VmValue> {
        vec![
            VmValue::Nil,
            string(authority_kind),
            string(&format!("sha256:{}", "a".repeat(64))),
            string(&plan_fingerprint()),
            string("hyp-1"),
            VmValue::Nil,
        ]
    }

    async fn mint_proof(
        authority_kind: &str,
        event_fingerprint: &str,
        plan_fingerprint: &str,
        run_id: Option<&str>,
    ) -> VmValue {
        let attestation = mint_hypothesis_native_attestation(
            authority_kind,
            event_fingerprint,
            plan_fingerprint,
            "hyp-1",
            run_id,
        )
        .expect("registered native adapter should issue an attestation");
        hypothesis_event_authority_mint_impl(
            ctx(),
            vec![
                attestation,
                string(authority_kind),
                string(event_fingerprint),
                string(plan_fingerprint),
                string("hyp-1"),
                run_id.map(string).unwrap_or(VmValue::Nil),
            ],
        )
        .await
        .expect("native authority mint should succeed")
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ordinary_harn_values_cannot_self_mint_native_authority() {
        let args = mint_policy_args("native_observation");
        for forged in [string("native-attestation"), dict([])] {
            let mut candidate = args.clone();
            candidate[0] = forged;
            let error = hypothesis_event_authority_mint_impl(ctx(), candidate)
                .await
                .expect_err("script values must not substitute for native adapter evidence");
            assert!(error
                .to_string()
                .contains("registered native adapter attestation resource"));
        }
    }

    fn append_args(
        proof: VmValue,
        payload: VmValue,
        event_fingerprint: &str,
        plan_fingerprint: &str,
        run_id: Option<&str>,
        expected_head: Option<&str>,
    ) -> Vec<VmValue> {
        vec![
            proof,
            string("hypothesis.plan_registered"),
            string(event_fingerprint),
            expected_head.map(string).unwrap_or(VmValue::Nil),
            string(event_fingerprint),
            string(plan_fingerprint),
            string("hyp-1"),
            run_id.map(string).unwrap_or(VmValue::Nil),
            payload,
            dict([]),
        ]
    }

    #[tokio::test(flavor = "current_thread")]
    async fn generic_public_emit_paths_reject_the_hypothesis_ledger_topic() {
        reset_active_event_log();
        install_memory_for_current_thread(EVENT_LOG_QUEUE_DEPTH);
        let cases = [
            event_log_emit_impl(
                ctx(),
                vec![string(HYPOTHESIS_LEDGER_TOPIC), string("forged")],
            )
            .await,
            event_log_emit_idempotent_impl(
                ctx(),
                vec![
                    string(HYPOTHESIS_LEDGER_TOPIC),
                    string("forged"),
                    string("event-1"),
                ],
            )
            .await,
            event_log_emit_idempotent_chained_impl(
                ctx(),
                vec![
                    string(HYPOTHESIS_LEDGER_TOPIC),
                    string("forged"),
                    string("event-1"),
                    VmValue::Nil,
                ],
            )
            .await,
        ];
        for result in cases {
            let error = result.expect_err("reserved topic must reject generic writes");
            assert!(error
                .to_string()
                .contains("reserved for native hypothesis authority"));
        }
        reset_active_event_log();
    }

    #[test]
    fn authority_mint_requires_the_exact_scoped_authority_effect() {
        let method = "hypothesis_event_authority_mint";
        let capability = harn_builtin_meta::CapabilityId::Observability;
        let connector_only = CapabilityPolicy {
            capabilities: BTreeMap::from([("connector".to_string(), vec!["call".to_string()])]),
            ..CapabilityPolicy::default()
        };
        push_execution_policy(connector_only);
        let connector_error = crate::orchestration::enforce_current_policy_for_capability(
            capability,
            method,
            &mint_policy_args("plan_admission"),
        )
        .expect_err("connector.call must not authorize a typed authority effect");
        assert!(connector_error
            .to_string()
            .contains("authority:write (plan_admission)"));
        pop_execution_policy();

        let exact = CapabilityPolicy {
            capabilities: BTreeMap::from([(
                "authority".to_string(),
                vec!["write@plan_admission".to_string()],
            )]),
            ..CapabilityPolicy::default()
        };
        push_execution_policy(exact);
        crate::orchestration::enforce_current_policy_for_capability(
            capability,
            method,
            &mint_policy_args("plan_admission"),
        )
        .expect("exact authority-effect scope should authorize the mint");
        crate::orchestration::enforce_current_policy_for_capability(
            capability,
            method,
            &mint_policy_args("native_approval"),
        )
        .expect_err("a plan-admission grant must not authorize native approval");
        pop_execution_policy();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn script_values_cannot_substitute_for_native_hypothesis_authority() {
        reset_active_event_log();
        install_memory_for_current_thread(EVENT_LOG_QUEUE_DEPTH);
        let (payload, event_fingerprint) = hypothesis_event_payload();
        let plan_fingerprint = plan_fingerprint();
        let forged = [
            string("hypothesis_event_authority"),
            dict([
                ("authority_kind", string("plan_admission")),
                ("event_fingerprint", string(&event_fingerprint)),
            ]),
        ];
        for proof in forged {
            let error = hypothesis_event_append_impl(
                ctx(),
                append_args(
                    proof,
                    payload.clone(),
                    &event_fingerprint,
                    &plan_fingerprint,
                    None,
                    None,
                ),
            )
            .await
            .expect_err("script-authored values must not carry authority");
            assert!(error
                .to_string()
                .contains("native hypothesis event authority resource"));
        }
        reset_active_event_log();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn exact_native_proof_appends_and_replays_idempotently() {
        reset_active_event_log();
        install_memory_for_current_thread(EVENT_LOG_QUEUE_DEPTH);
        let execution_scope = mint_execution_scope();
        let _scope = enter_execution_scope(execution_scope);
        let (payload, event_fingerprint) = hypothesis_event_payload();
        let plan_fingerprint = plan_fingerprint();
        let proof = mint_proof(
            "plan_admission",
            &event_fingerprint,
            &plan_fingerprint,
            None,
        )
        .await;

        let inserted = hypothesis_event_append_impl(
            ctx(),
            append_args(
                proof.clone(),
                payload.clone(),
                &event_fingerprint,
                &plan_fingerprint,
                None,
                None,
            ),
        )
        .await
        .expect("exact proof append should succeed");
        assert_eq!(vm_value_to_json(&inserted)["inserted"], true);

        let mut alternate_key = append_args(
            proof.clone(),
            payload.clone(),
            &event_fingerprint,
            &plan_fingerprint,
            None,
            None,
        );
        alternate_key[2] = string("alternate-key");
        hypothesis_event_append_impl(ctx(), alternate_key)
            .await
            .expect_err("one proof cannot insert the same event under another identity");

        let replay = hypothesis_event_append_impl(
            ctx(),
            append_args(
                proof,
                payload,
                &event_fingerprint,
                &plan_fingerprint,
                None,
                Some("sha256:deliberately-stale"),
            ),
        )
        .await
        .expect("exact idempotent replay should ignore a stale head");
        assert_eq!(vm_value_to_json(&replay)["inserted"], false);

        let topic = Topic::new(HYPOTHESIS_LEDGER_TOPIC).unwrap();
        let events = ensure_event_log()
            .read_range(&topic, None, usize::MAX)
            .await
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].1.headers[HYPOTHESIS_AUTHORITY_KIND_HEADER],
            "plan_admission"
        );
        assert_eq!(
            events[0].1.headers[HYPOTHESIS_EVENT_FINGERPRINT_HEADER],
            event_fingerprint
        );
        reset_active_event_log();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn native_replay_revalidates_the_complete_projection_headers() {
        reset_active_event_log();
        install_memory_for_current_thread(EVENT_LOG_QUEUE_DEPTH);
        let _scope = enter_execution_scope(mint_execution_scope());
        let (payload, event_fingerprint) = hypothesis_event_payload();
        let plan_fingerprint = plan_fingerprint();
        let proof = mint_proof(
            "plan_admission",
            &event_fingerprint,
            &plan_fingerprint,
            None,
        )
        .await;
        let native = hypothesis_authority_proof(Some(&proof), "test").unwrap();
        let mut headers = BTreeMap::new();
        headers.insert(IDEMPOTENCY_HEADER.to_string(), event_fingerprint.clone());
        insert_hypothesis_authority_headers(&mut headers, &native);
        headers.insert("schema".to_string(), "harn.hypothesis.event.v1".to_string());
        headers.insert("event_id".to_string(), "tampered-event-id".to_string());
        headers.insert("hypothesis_id".to_string(), "hyp-1".to_string());
        headers.insert("fingerprint".to_string(), event_fingerprint.clone());
        let topic = Topic::new(HYPOTHESIS_LEDGER_TOPIC).unwrap();
        ensure_event_log()
            .append_idempotent_by_header(
                &topic,
                IDEMPOTENCY_HEADER,
                &event_fingerprint,
                LogEvent::new("hypothesis.plan_registered", vm_value_to_json(&payload))
                    .with_headers(headers),
            )
            .await
            .unwrap();

        hypothesis_event_append_impl(
            ctx(),
            append_args(
                proof,
                payload,
                &event_fingerprint,
                &plan_fingerprint,
                None,
                Some("sha256:stale-replay-head"),
            ),
        )
        .await
        .expect_err("a replay with tampered projection headers must fail closed");
        reset_active_event_log();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn native_proof_rejects_tampered_fingerprint_plan_and_run_bindings() {
        reset_active_event_log();
        install_memory_for_current_thread(EVENT_LOG_QUEUE_DEPTH);
        let execution_scope = mint_execution_scope();
        let _scope = enter_execution_scope(execution_scope);
        let (payload, event_fingerprint) = hypothesis_event_payload();
        let plan_fingerprint = plan_fingerprint();
        let proof = mint_proof(
            "native_observation",
            &event_fingerprint,
            &plan_fingerprint,
            Some("run-1"),
        )
        .await;
        let altered_fingerprint = format!("sha256:{}", "c".repeat(64));
        let altered_plan = format!("sha256:{}", "d".repeat(64));

        let cases = [
            (
                altered_fingerprint.as_str(),
                plan_fingerprint.as_str(),
                Some("run-1"),
                "event fingerprint",
            ),
            (
                event_fingerprint.as_str(),
                altered_plan.as_str(),
                Some("run-1"),
                "plan fingerprint",
            ),
            (
                event_fingerprint.as_str(),
                plan_fingerprint.as_str(),
                Some("run-2"),
                "run id",
            ),
        ];
        for (candidate_event, candidate_plan, candidate_run, expected_error) in cases {
            let error = hypothesis_event_append_impl(
                ctx(),
                append_args(
                    proof.clone(),
                    payload.clone(),
                    candidate_event,
                    candidate_plan,
                    candidate_run,
                    None,
                ),
            )
            .await
            .expect_err("tampered binding must fail");
            assert!(error.to_string().contains(expected_error), "{error}");
        }
        let mut altered_hypothesis_args = append_args(
            proof,
            payload,
            &event_fingerprint,
            &plan_fingerprint,
            Some("run-1"),
            None,
        );
        altered_hypothesis_args[6] = string("hyp-2");
        let hypothesis_error = hypothesis_event_append_impl(ctx(), altered_hypothesis_args)
            .await
            .expect_err("tampered hypothesis id must fail");
        assert!(hypothesis_error.to_string().contains("hypothesis id"));
        reset_active_event_log();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn native_proof_rejects_payload_content_tampered_after_mint() {
        reset_active_event_log();
        install_memory_for_current_thread(EVENT_LOG_QUEUE_DEPTH);
        let execution_scope = mint_execution_scope();
        let _scope = enter_execution_scope(execution_scope);
        let (_payload, event_fingerprint) = hypothesis_event_payload();
        let plan_fingerprint = plan_fingerprint();
        let proof = mint_proof(
            "plan_admission",
            &event_fingerprint,
            &plan_fingerprint,
            None,
        )
        .await;
        let tampered_payload = dict([
            (
                "content",
                dict([
                    ("event_id", string("event-2")),
                    ("hypothesis_id", string("hyp-1")),
                ]),
            ),
            ("fingerprint", string(&event_fingerprint)),
        ]);

        let error = hypothesis_event_append_impl(
            ctx(),
            append_args(
                proof,
                tampered_payload,
                &event_fingerprint,
                &plan_fingerprint,
                None,
                None,
            ),
        )
        .await
        .expect_err("authorized fingerprint must not bless altered content");
        assert!(error.to_string().contains("payload content does not match"));
        reset_active_event_log();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn native_proof_rejects_a_cross_lane_event_projection() {
        reset_active_event_log();
        install_memory_for_current_thread(EVENT_LOG_QUEUE_DEPTH);
        let execution_scope = mint_execution_scope();
        let _scope = enter_execution_scope(execution_scope);
        let (payload, event_fingerprint) = hypothesis_event_payload();
        let plan_fingerprint = plan_fingerprint();
        let proof = mint_proof(
            "plan_admission",
            &event_fingerprint,
            &plan_fingerprint,
            None,
        )
        .await;
        let mut args = append_args(
            proof,
            payload,
            &event_fingerprint,
            &plan_fingerprint,
            None,
            None,
        );
        args[1] = string("hypothesis.observation_recorded");
        let error = hypothesis_event_append_impl(ctx(), args)
            .await
            .expect_err("plan admission must not authorize an observation projection");
        assert!(error.to_string().contains("native authority lane"));
        reset_active_event_log();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn native_proof_cannot_cross_execution_scopes() {
        reset_active_event_log();
        install_memory_for_current_thread(EVENT_LOG_QUEUE_DEPTH);
        let (payload, event_fingerprint) = hypothesis_event_payload();
        let plan_fingerprint = plan_fingerprint();
        let proof = {
            let _scope = enter_execution_scope(mint_execution_scope());
            mint_proof(
                "plan_admission",
                &event_fingerprint,
                &plan_fingerprint,
                None,
            )
            .await
        };
        let _other_scope = enter_execution_scope(mint_execution_scope());
        let error = hypothesis_event_append_impl(
            ctx(),
            append_args(
                proof,
                payload,
                &event_fingerprint,
                &plan_fingerprint,
                None,
                None,
            ),
        )
        .await
        .expect_err("proof from another execution must fail");
        assert!(error.to_string().contains("different execution scope"));
        reset_active_event_log();
    }
}
