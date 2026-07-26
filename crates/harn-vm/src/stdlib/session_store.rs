//! `std/session-store` adapter over the canonical `harn-session-store` crate.
//!
//! SQLite owns event allocation, canonical hashes, persistence, and
//! verification. This module only maps Harn values to that typed contract and
//! imports the retired per-session JSONL format on first access.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use harn_session_store::{
    AppendEvent, CreateSession, EventId, EventIdentity, EventIdentityField, ImportSession,
    ReadRange, SessionEventKind, SessionImporter, SessionStore, SqliteSessionStore, StoreError,
    StoreHooks, StoredEvent, VerifyReport, MAX_READ_BATCH,
};
use serde::Deserialize;
use serde_json::{json, Value as JsonValue};
use sha2::{Digest, Sha256};

use crate::llm::vm_value_to_json;
use crate::stdlib::json_to_vm_value;
use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::stdlib::options::{optional_dict_arg, required_string_arg, ErrorKind};
use crate::value::{categorized_error, DictMap, ErrorCategory, VmError, VmValue};
use crate::vm::Vm;

const LEGACY_STORE_DIR: &str = "session-store";
const LEGACY_EVENT_FILE_EXT: &str = "jsonl";
const STORE_DB_FILE: &str = "session-store.sqlite";
/// Directory name for state under an explicitly named store root. The
/// defaulted case goes through `runtime_paths::state_root` instead, which
/// owns the same name plus the `HARN_STATE_DIR` override.
const STATE_DIR_NAME: &str = ".harn";

/// The directory session-store files live in.
///
/// Deliberately not a bare `PathBuf`. Callers naturally hold the *tree root*
/// — a temp dir, a project root — and the state directory is one level below
/// it. Both are paths, so passing one where the other is meant compiles
/// silently and fails at runtime as an empty store. Naming the distinction in
/// the type makes that mistake a compile error, and
/// [`SessionStoreDir::under_root`] is the only way to descend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SessionStoreDir(PathBuf);

impl SessionStoreDir {
    /// State for an explicitly named tree: `<root>/.harn`.
    pub(crate) fn under_root(root: &Path) -> Self {
        Self(root.join(STATE_DIR_NAME))
    }

    pub(crate) fn as_path(&self) -> &Path {
        &self.0
    }
}
const STORE_ROOT_ENV: &str = "HARN_SESSION_STORE_ROOT";
const DEFAULT_ACTOR: &str = "harn";
const DEFAULT_KIND_TYPE: &str = "LibrarianEntry";
const LEGACY_IMPORT_FORMAT: &str = "harn.session.jsonl.v1";

pub(crate) fn register_session_store_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    &SESSION_STORE_APPEND_IMPL_DEF,
    &SESSION_STORE_EVENTS_IMPL_DEF,
    &SESSION_STORE_VERIFY_IMPL_DEF,
    &SESSION_STORE_PATH_IMPL_DEF,
    &SESSION_STORE_DATABASE_PATH_IMPL_DEF,
];

#[harn_builtin(
    sig = "__session_store_append(session_id: string, payload: any, options?: dict) -> dict",
    kind = "async",
    category = "session_store"
)]
async fn session_store_append_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let session_id = required_string_arg(
        &args,
        0,
        "__session_store_append",
        "session_id",
        ErrorKind::Runtime,
    )?
    .trim()
    .to_string();
    let payload = args.get(1).map(vm_value_to_json).unwrap_or(JsonValue::Null);
    let options = optional_dict_arg(
        &args,
        2,
        "__session_store_append",
        "options",
        ErrorKind::Runtime,
    )?;
    reject_retired_now(options)?;
    let state_dir = store_state_dir(options)?;
    let store = open_store(&state_dir)?;
    let tenant_id = option_string(options, "tenant_id")?;
    ensure_session(&store, &state_dir, &session_id, true, tenant_id).await?;

    let kind = match option_string(options, "kind")?.as_deref() {
        Some("hypothesis") => SessionEventKind::Hypothesis,
        _ => SessionEventKind::Custom {
            custom_type: option_string(options, "kind_type")?
                .unwrap_or_else(|| DEFAULT_KIND_TYPE.to_string()),
        },
    };
    let meta = store.describe(&session_id).await.map_err(store_error)?;
    let mut event = AppendEvent::new(kind, payload)
        .with_actor(option_string(options, "actor")?.unwrap_or_else(|| DEFAULT_ACTOR.to_string()))
        .with_tags(option_tags(options)?);
    if let Some(parent_event_id) = meta.last_event_id {
        event = event.with_parent(parent_event_id);
    }
    event.headers = option_headers(options)?;
    let stored = store
        .append(&session_id, event)
        .await
        .map_err(store_error)?;
    stored_event_value(stored)
}

#[harn_builtin(
    sig = "__session_store_events(session_id: string, options?: dict) -> list",
    kind = "async",
    category = "session_store"
)]
async fn session_store_events_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let session_id = required_string_arg(
        &args,
        0,
        "__session_store_events",
        "session_id",
        ErrorKind::Runtime,
    )?
    .trim()
    .to_string();
    let options = optional_dict_arg(
        &args,
        1,
        "__session_store_events",
        "options",
        ErrorKind::Runtime,
    )?;
    let state_dir = store_state_dir(options)?;
    let store = open_store(&state_dir)?;
    let tenant_id = option_string(options, "tenant_id")?;
    if !ensure_session(&store, &state_dir, &session_id, false, tenant_id).await? {
        return Ok(VmValue::List(Arc::new(Vec::new())));
    }
    let events = read_all_events(&store, &session_id).await?;
    let values = events
        .into_iter()
        .map(stored_event_value)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(VmValue::List(Arc::new(values)))
}

#[harn_builtin(
    sig = "__session_store_verify(session_id: string, options?: dict) -> dict",
    kind = "async",
    category = "session_store"
)]
async fn session_store_verify_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let session_id = required_string_arg(
        &args,
        0,
        "__session_store_verify",
        "session_id",
        ErrorKind::Runtime,
    )?
    .trim()
    .to_string();
    let options = optional_dict_arg(
        &args,
        1,
        "__session_store_verify",
        "options",
        ErrorKind::Runtime,
    )?;
    let state_dir = store_state_dir(options)?;
    let store = open_store(&state_dir)?;
    let tenant_id = option_string(options, "tenant_id")?;
    if !ensure_session(&store, &state_dir, &session_id, false, tenant_id).await? {
        return Ok(json_to_vm_value(&json!({
            "_type": "session_store_verify",
            "session_id": session_id,
            "ok": true,
            "count": 0,
            "signed_event_count": 0,
        })));
    }
    let report = store.verify(&session_id).await.map_err(store_error)?;
    let broken_at = if let Some(failure) = report.failures.first() {
        read_all_events(&store, &session_id)
            .await?
            .iter()
            .position(|event| event.event_id == failure.event_id)
            .or(Some(0))
    } else {
        None
    };
    verify_report_value(report, broken_at)
}

#[harn_builtin(
    sig = "__session_store_path(session_id: string, options?: dict) -> string",
    kind = "async",
    category = "session_store"
)]
async fn session_store_path_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let session_id = required_string_arg(
        &args,
        0,
        "__session_store_path",
        "session_id",
        ErrorKind::Runtime,
    )?
    .trim()
    .to_string();
    validate_session_id(&session_id)?;
    let options = optional_dict_arg(
        &args,
        1,
        "__session_store_path",
        "options",
        ErrorKind::Runtime,
    )?;
    Ok(VmValue::String(arcstr::ArcStr::from(
        legacy_session_path(&store_state_dir(options)?, &session_id)?
            .to_string_lossy()
            .as_ref(),
    )))
}

#[harn_builtin(
    sig = "__session_store_database_path(options?: dict) -> string",
    kind = "async",
    category = "session_store"
)]
async fn session_store_database_path_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let options = optional_dict_arg(
        &args,
        0,
        "__session_store_database_path",
        "options",
        ErrorKind::Runtime,
    )?;
    Ok(VmValue::String(arcstr::ArcStr::from(
        store_path(&store_state_dir(options)?)
            .to_string_lossy()
            .as_ref(),
    )))
}

fn open_store(state_dir: &SessionStoreDir) -> Result<SqliteSessionStore, VmError> {
    let hooks = StoreHooks {
        redaction: Some(Arc::new(crate::redact::current_policy())),
        ..StoreHooks::default()
    };
    SqliteSessionStore::open_with_hooks(store_path(state_dir), hooks).map_err(store_error)
}

fn store_path(state_dir: &SessionStoreDir) -> PathBuf {
    state_dir.as_path().join(STORE_DB_FILE)
}

async fn ensure_session(
    store: &SqliteSessionStore,
    state_dir: &SessionStoreDir,
    session_id: &str,
    create_if_missing: bool,
    tenant_id: Option<String>,
) -> Result<bool, VmError> {
    validate_session_id(session_id)?;
    let legacy_path = legacy_session_path(state_dir, session_id)?;
    if legacy_path.is_file() {
        let source = read_legacy_events(state_dir, session_id)?;
        if !source.events.is_empty() {
            import_legacy_events(store, session_id, source, tenant_id.as_deref()).await?;
            return match store.describe(session_id).await {
                Ok(meta) => {
                    validate_tenant(session_id, &meta.tenant_id, tenant_id.as_deref())?;
                    Ok(true)
                }
                Err(StoreError::NotFound(_)) => Err(VmError::Runtime(format!(
                    "session_store: legacy source for '{session_id}' was already imported and the canonical session was deleted"
                ))),
                Err(error) => Err(store_error(error)),
            };
        }
    }
    match store.describe(session_id).await {
        Ok(meta) => {
            validate_tenant(session_id, &meta.tenant_id, tenant_id.as_deref())?;
            Ok(true)
        }
        Err(StoreError::NotFound(_)) => {
            if !create_if_missing {
                return Ok(false);
            }
            match store
                .create(CreateSession {
                    id: Some(session_id.to_string()),
                    tenant_id: tenant_id.clone(),
                    ..CreateSession::default()
                })
                .await
            {
                Ok(_) => {}
                Err(StoreError::AlreadyExists(_)) => {}
                Err(error) => return Err(store_error(error)),
            }
            let meta = store.describe(session_id).await.map_err(store_error)?;
            validate_tenant(session_id, &meta.tenant_id, tenant_id.as_deref())?;
            Ok(true)
        }
        Err(error) => Err(store_error(error)),
    }
}

/// Open the canonical store for a VM-owned agent session. The live transcript
/// journal uses this adapter instead of reproducing SQLite setup, redaction,
/// legacy import, or session-creation policy.
pub(crate) async fn open_canonical_agent_session(
    state_dir: &SessionStoreDir,
    session_id: &str,
) -> Result<SqliteSessionStore, VmError> {
    let store = open_store(state_dir)?;
    ensure_session(&store, state_dir, session_id, true, None).await?;
    Ok(store)
}

pub(crate) async fn read_all_events(
    store: &SqliteSessionStore,
    session_id: &str,
) -> Result<Vec<StoredEvent>, VmError> {
    let mut events = Vec::new();
    let mut cursor: Option<EventId> = None;
    loop {
        let page = store
            .read(
                session_id,
                ReadRange {
                    from_event_id: cursor,
                    to_event_id: None,
                    limit: Some(MAX_READ_BATCH),
                },
            )
            .await
            .map_err(store_error)?;
        cursor = page.next_cursor;
        events.extend(page.events);
        if cursor.is_none() {
            return Ok(events);
        }
    }
}

#[derive(Debug, Deserialize)]
struct LegacyEvent {
    event_id: EventId,
    session_id: String,
    #[serde(default)]
    tenant_id: Option<JsonValue>,
    #[serde(default)]
    parent_event_id: Option<EventId>,
    #[serde(default)]
    actor: Option<String>,
    kind: SessionEventKind,
    #[serde(default)]
    payload: JsonValue,
    #[serde(default)]
    tags: Vec<JsonValue>,
    #[serde(default)]
    headers: BTreeMap<String, JsonValue>,
    ts_ms: i64,
    ts: String,
    record_hash: String,
    #[serde(default)]
    prev_hash: Option<String>,
}

#[derive(Debug)]
struct LegacySource {
    digest: String,
    events: Vec<LegacyEvent>,
}

fn read_legacy_events(
    state_dir: &SessionStoreDir,
    session_id: &str,
) -> Result<LegacySource, VmError> {
    let path = legacy_session_path(state_dir, session_id)?;
    let bytes = fs::read(&path).map_err(|error| {
        VmError::Runtime(format!(
            "session_store: failed to read legacy {}: {error}",
            path.display()
        ))
    })?;
    let text = std::str::from_utf8(&bytes).map_err(|error| {
        VmError::Runtime(format!(
            "session_store: legacy {} is not UTF-8: {error}",
            path.display()
        ))
    })?;
    let mut events = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let event = serde_json::from_str::<LegacyEvent>(line).map_err(|error| {
            VmError::Runtime(format!(
                "session_store: invalid legacy event at {}:{}: {error}",
                path.display(),
                index + 1
            ))
        })?;
        if event.session_id != session_id {
            return Err(VmError::Runtime(format!(
                "session_store: legacy event at {}:{} belongs to '{}' not '{session_id}'",
                path.display(),
                index + 1,
                event.session_id
            )));
        }
        events.push(event);
    }
    validate_legacy_events(&events)?;
    Ok(LegacySource {
        digest: format!("sha256:{}", hex::encode(Sha256::digest(&bytes))),
        events,
    })
}

async fn import_legacy_events(
    store: &SqliteSessionStore,
    session_id: &str,
    source: LegacySource,
    requested_tenant: Option<&str>,
) -> Result<(), VmError> {
    let tenant_id = source
        .events
        .first()
        .map(legacy_tenant)
        .transpose()?
        .flatten();
    validate_tenant(session_id, &tenant_id, requested_tenant)?;

    let mut events = Vec::with_capacity(source.events.len());
    for legacy in source.events {
        let mut redacted_headers = serde_json::to_value(&legacy.headers).map_err(|error| {
            VmError::Runtime(format!(
                "session_store: failed to preserve legacy headers: {error}"
            ))
        })?;
        if let Some(redactor) = store.hooks().redaction.as_ref() {
            redactor.redact_json_in_place(&mut redacted_headers);
        }
        let original_headers = serde_json::to_string(&redacted_headers).map_err(|error| {
            VmError::Runtime(format!(
                "session_store: failed to preserve legacy headers: {error}"
            ))
        })?;
        let mut headers = serde_json::from_value::<BTreeMap<String, JsonValue>>(redacted_headers)
            .map_err(|error| {
                VmError::Runtime(format!(
                    "session_store: failed to project redacted legacy headers: {error}"
                ))
            })?
            .into_iter()
            .map(|(key, value)| (key, json_header_value(&value)))
            .collect::<BTreeMap<_, _>>();
        normalize_legacy_identity_headers(&mut headers, legacy.event_id)?;
        headers.insert("harn.legacy.original_headers".to_string(), original_headers);
        headers.insert(
            "harn.legacy.event_id".to_string(),
            legacy.event_id.to_string(),
        );
        headers.insert("harn.legacy.record_hash".to_string(), legacy.record_hash);
        headers.insert("harn.legacy.ts_ms".to_string(), legacy.ts_ms.to_string());
        headers.insert("harn.legacy.ts".to_string(), legacy.ts);
        if let Some(prev_hash) = legacy.prev_hash {
            headers.insert("harn.legacy.prev_hash".to_string(), prev_hash);
        }
        if let Some(tenant_id) = legacy.tenant_id.as_ref().and_then(json_scalar_string) {
            headers.insert("harn.legacy.tenant_id".to_string(), tenant_id);
        }
        let mut event = AppendEvent::new(legacy.kind, legacy.payload)
            .with_actor(legacy.actor.unwrap_or_else(|| DEFAULT_ACTOR.to_string()))
            .with_tags(legacy.tags.iter().map(json_header_value));
        event.headers = headers;
        if let Some(parent_event_id) = legacy.parent_event_id {
            event = event.with_parent(parent_event_id);
        }
        events.push(event);
    }
    store
        .import(ImportSession {
            source_id: format!("{LEGACY_IMPORT_FORMAT}:{session_id}"),
            source_digest: source.digest,
            session: CreateSession {
                id: Some(session_id.to_string()),
                tenant_id,
                ..CreateSession::default()
            },
            events,
        })
        .await
        .map_err(store_error)?;
    Ok(())
}

fn stored_event_value(event: StoredEvent) -> Result<VmValue, VmError> {
    let value = serde_json::to_value(event).map_err(|error| {
        VmError::Runtime(format!(
            "session_store: failed to encode stored event: {error}"
        ))
    })?;
    Ok(json_to_vm_value(&value))
}

fn verify_report_value(report: VerifyReport, broken_at: Option<usize>) -> Result<VmValue, VmError> {
    let reason = report
        .failures
        .first()
        .map(|failure| failure.reason.clone());
    let value = json!({
        "_type": "session_store_verify",
        "session_id": report.session_id,
        "ok": report.failures.is_empty(),
        "count": report.event_count,
        "signed_event_count": report.signed_event_count,
        "chain_root_hash": report.chain_root_hash,
        "broken_at": broken_at,
        "reason": reason,
    });
    Ok(json_to_vm_value(&value))
}

/// The directory session-store files live in.
///
/// A caller that names a tree — via the `root` option or `HARN_SESSION_STORE_ROOT`
/// — gets that tree's `.harn`, because it asked for a specific location. With
/// neither, the location is Harn's own default, so it goes through
/// [`crate::runtime_paths::state_root`] and honors `HARN_STATE_DIR` like every
/// other state consumer. Resolving both cases here keeps one owner for the
/// question; downstream helpers only join file names onto the result.
pub(crate) fn canonical_store_state_dir(
    options: Option<&DictMap>,
) -> Result<SessionStoreDir, VmError> {
    let named_root = option_string(options, "root")?
        .map(|root| crate::stdlib::process::resolve_source_relative_path(&root))
        .or_else(|| {
            crate::stdlib::process::read_env_value(STORE_ROOT_ENV)
                .filter(|value| !value.trim().is_empty())
                .map(|root| crate::stdlib::process::resolve_source_relative_path(&root))
        });
    Ok(match named_root {
        Some(root) => SessionStoreDir::under_root(&root),
        None => SessionStoreDir(crate::runtime_paths::state_root(
            &crate::stdlib::process::runtime_root_base(),
        )),
    })
}

fn store_state_dir(options: Option<&DictMap>) -> Result<SessionStoreDir, VmError> {
    canonical_store_state_dir(options)
}

fn legacy_session_path(state_dir: &SessionStoreDir, session_id: &str) -> Result<PathBuf, VmError> {
    validate_session_id(session_id)?;
    Ok(state_dir
        .as_path()
        .join(LEGACY_STORE_DIR)
        .join(format!("{session_id}.{LEGACY_EVENT_FILE_EXT}")))
}

fn validate_session_id(session_id: &str) -> Result<(), VmError> {
    let trimmed = session_id.trim();
    if trimmed.is_empty() {
        return Err(VmError::Runtime(
            "session_store: session_id must be non-empty".to_string(),
        ));
    }
    if trimmed == "." || trimmed == ".." || trimmed.contains("..") {
        return Err(VmError::Runtime(
            "session_store: session_id must not contain `..`".to_string(),
        ));
    }
    if trimmed.contains('/') || trimmed.contains('\\') || trimmed.contains('\0') {
        return Err(VmError::Runtime(
            "session_store: session_id must not contain path separators".to_string(),
        ));
    }
    Ok(())
}

fn option_string(options: Option<&DictMap>, key: &str) -> Result<Option<String>, VmError> {
    match options.and_then(|options| options.get(key)) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::String(value)) if value.trim().is_empty() => Ok(None),
        Some(VmValue::String(value)) => Ok(Some(value.to_string())),
        Some(_) => Err(VmError::Runtime(format!(
            "session_store: options.{key} must be a string"
        ))),
    }
}

fn reject_retired_now(options: Option<&DictMap>) -> Result<(), VmError> {
    if options.is_some_and(|options| options.contains_key("now")) {
        return Err(VmError::Runtime(
            "session_store: options.now is no longer supported; canonical timestamps are assigned by the store"
                .to_string(),
        ));
    }
    Ok(())
}

fn option_tags(options: Option<&DictMap>) -> Result<Vec<String>, VmError> {
    match options.and_then(|options| options.get("tags")) {
        None | Some(VmValue::Nil) => Ok(Vec::new()),
        Some(VmValue::List(items)) => items
            .iter()
            .map(|item| match item {
                VmValue::String(value) => Ok(value.to_string()),
                _ => Err(VmError::Runtime(
                    "session_store: options.tags must contain only strings".to_string(),
                )),
            })
            .collect(),
        Some(_) => Err(VmError::Runtime(
            "session_store: options.tags must be a list".to_string(),
        )),
    }
}

fn option_headers(options: Option<&DictMap>) -> Result<BTreeMap<String, String>, VmError> {
    match options.and_then(|options| options.get("headers")) {
        None | Some(VmValue::Nil) => Ok(BTreeMap::new()),
        Some(VmValue::Dict(headers)) => headers
            .iter()
            .map(|(key, value)| match value {
                VmValue::String(value) => Ok((key.to_string(), value.to_string())),
                _ => Err(VmError::Runtime(
                    "session_store: options.headers values must be strings".to_string(),
                )),
            })
            .collect(),
        Some(_) => Err(VmError::Runtime(
            "session_store: options.headers must be a dict".to_string(),
        )),
    }
}

fn json_scalar_string(value: &JsonValue) -> Option<String> {
    match value {
        JsonValue::Null => None,
        JsonValue::String(value) => Some(value.clone()),
        other => Some(other.to_string()),
    }
}

fn json_header_value(value: &JsonValue) -> String {
    json_scalar_string(value).unwrap_or_default()
}

fn legacy_tenant(event: &LegacyEvent) -> Result<Option<String>, VmError> {
    match event.tenant_id.as_ref() {
        None | Some(JsonValue::Null) => Ok(None),
        Some(JsonValue::String(value)) if !value.trim().is_empty() => Ok(Some(value.clone())),
        Some(_) => Err(VmError::Runtime(format!(
            "session_store: legacy tenant_id at event {} must be a non-empty string or null",
            event.event_id
        ))),
    }
}

fn validate_legacy_events(events: &[LegacyEvent]) -> Result<(), VmError> {
    let mut seen = BTreeSet::new();
    let mut expected_prev_hash: Option<&str> = None;
    let mut tenant_id: Option<Option<String>> = None;
    for (index, event) in events.iter().enumerate() {
        let expected_event_id = index as EventId + 1;
        if event.event_id != expected_event_id {
            return Err(VmError::Runtime(format!(
                "session_store: legacy event_id sequence gap: expected {expected_event_id}, found {}",
                event.event_id
            )));
        }
        if !seen.insert(event.event_id) {
            return Err(VmError::Runtime(format!(
                "session_store: duplicate legacy event_id {}",
                event.event_id
            )));
        }
        if let Some(parent_event_id) = event.parent_event_id {
            if !seen.contains(&parent_event_id) {
                return Err(VmError::Runtime(format!(
                    "session_store: legacy parent_event_id {parent_event_id} is not an earlier event"
                )));
            }
        }
        if event.prev_hash.as_deref() != expected_prev_hash {
            return Err(VmError::Runtime(format!(
                "session_store: legacy prev_hash chain break at event {}",
                event.event_id
            )));
        }
        let computed = legacy_record_hash(event)?;
        if event.record_hash != computed {
            return Err(VmError::Runtime(format!(
                "session_store: legacy record_hash mismatch at event {}",
                event.event_id
            )));
        }
        let event_tenant = legacy_tenant(event)?;
        match &tenant_id {
            Some(expected) if expected != &event_tenant => {
                return Err(VmError::Runtime(format!(
                    "session_store: mixed tenant_id values in legacy stream at event {}",
                    event.event_id
                )))
            }
            None => tenant_id = Some(event_tenant),
            _ => {}
        }
        expected_prev_hash = Some(event.record_hash.as_str());
    }
    Ok(())
}

fn legacy_record_hash(event: &LegacyEvent) -> Result<String, VmError> {
    let canonical = serde_json::to_string(&BTreeMap::from([
        ("event_id", json!(event.event_id)),
        ("payload", event.payload.clone()),
        ("prev_hash", json!(event.prev_hash.clone())),
        ("session_id", json!(event.session_id.clone())),
    ]))
    .map_err(|error| {
        VmError::Runtime(format!(
            "session_store: failed to verify legacy event {}: {error}",
            event.event_id
        ))
    })?;
    Ok(format!(
        "sha256:{}",
        hex::encode(Sha256::digest(canonical.as_bytes()))
    ))
}

fn validate_tenant(
    session_id: &str,
    stored: &Option<String>,
    requested: Option<&str>,
) -> Result<(), VmError> {
    let Some(requested) = requested else {
        return Ok(());
    };
    if stored.as_deref() == Some(requested) {
        return Ok(());
    }
    Err(VmError::Runtime(format!(
        "session_store: tenant_id '{requested}' does not match stream '{session_id}' tenant {:?}",
        stored.as_deref()
    )))
}

fn normalize_legacy_identity_headers(
    headers: &mut BTreeMap<String, String>,
    legacy_event_id: EventId,
) -> Result<(), VmError> {
    let mut identity = EventIdentity::new();
    for field in EventIdentityField::ALL {
        let key = field.header_name();
        let Some(value) = headers.remove(key) else {
            continue;
        };
        let _ = identity.insert(field, value);
    }
    if identity.get(EventIdentityField::SourceEventId).is_none() {
        identity
            .insert(
                EventIdentityField::SourceEventId,
                legacy_event_id.to_string(),
            )
            .map_err(|error| VmError::Runtime(format!("session_store: {error}")))?;
    }
    identity
        .apply_to_headers(headers)
        .map_err(|error| VmError::Runtime(format!("session_store: {error}")))
}

fn store_error(error: StoreError) -> VmError {
    let message = format!("session_store: {error}");
    match error {
        StoreError::Contention { .. } => categorized_error(message, ErrorCategory::ResourceBusy),
        _ => VmError::Runtime(message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harn_session_store::StoreContention;

    #[test]
    fn store_contention_maps_to_typed_transient_category() {
        let error = store_error(StoreError::Contention {
            kind: StoreContention::DatabaseBusy,
            message: "database is locked".to_string(),
        });

        assert_eq!(
            crate::value::error_to_category(&error),
            ErrorCategory::ResourceBusy
        );
        assert!(ErrorCategory::ResourceBusy.is_transient());
        assert_eq!(
            error.to_string(),
            "Error [resource_busy]: session_store: retryable backend contention (database_busy): database is locked"
        );
    }

    fn write_legacy_fixture(state_dir: &SessionStoreDir, session_id: &str) -> PathBuf {
        let path = legacy_session_path(state_dir, session_id).unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let rows = [
            json!({
                "event_id": 1,
                "session_id": session_id,
                "tenant_id": "tenant-a",
                "parent_event_id": null,
                "actor": "harn",
                "kind": {"kind": "custom", "type": "AgentMemory"},
                "payload": {"operation": "upsert", "record": {"id": "a"}},
                "tags": ["legacy"],
                "headers": {
                    "run_id": " ",
                    "authorization": "Bearer source-secret",
                    "x-debug": "sk-proj-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "x-source": 7,
                    "harn.legacy.event_id": "source-value",
                    "harn.legacy.header.run_id": "source-relocation-value",
                },
                "ts_ms": 1000,
                "ts": "1970-01-01T00:00:01Z",
                "record_hash": "sha256:placeholder",
                "prev_hash": null,
            }),
            json!({
                "event_id": 2,
                "session_id": session_id,
                "tenant_id": "tenant-a",
                "parent_event_id": 1,
                "actor": "harn",
                "kind": {"kind": "hypothesis"},
                "payload": {"operation": "delete", "id": "a"},
                "tags": [],
                "headers": {},
                "ts_ms": 2000,
                "ts": "1970-01-01T00:00:02Z",
                "record_hash": "sha256:placeholder",
                "prev_hash": null,
            }),
        ];
        let mut rows = rows;
        let first: LegacyEvent = serde_json::from_value(rows[0].clone()).unwrap();
        let first_hash = legacy_record_hash(&first).unwrap();
        rows[0]["record_hash"] = json!(first_hash);
        rows[1]["prev_hash"] = rows[0]["record_hash"].clone();
        let second: LegacyEvent = serde_json::from_value(rows[1].clone()).unwrap();
        rows[1]["record_hash"] = json!(legacy_record_hash(&second).unwrap());
        let body = rows
            .iter()
            .map(JsonValue::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn stream_id_escape_is_rejected() {
        assert!(validate_session_id("../evil").is_err());
        assert!(validate_session_id("a/b").is_err());
        assert!(validate_session_id("burin.agent_context.memories.project").is_ok());
    }

    #[test]
    fn a_named_root_keeps_its_own_state_dir_regardless_of_harn_state_dir() {
        let _guard = crate::runtime_paths::test_env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let elsewhere = std::env::temp_dir().join("harn-session-store-state-dir-probe");
        let previous = std::env::var(crate::runtime_paths::HARN_STATE_DIR_ENV).ok();
        unsafe { std::env::set_var(crate::runtime_paths::HARN_STATE_DIR_ENV, &elsewhere) };

        // A caller that names a tree asked for a specific location; an
        // unrelated HARN_STATE_DIR must not relocate its store out of it.
        let options = json_to_vm_value(&json!({"root": "/workspace"}));
        let named = canonical_store_state_dir(options.as_dict()).unwrap();
        assert_eq!(named, SessionStoreDir::under_root(Path::new("/workspace")));

        // The defaulted case is Harn's own state, so it follows HARN_STATE_DIR
        // like every other state consumer.
        let defaulted = canonical_store_state_dir(None).unwrap();
        assert_eq!(defaulted.as_path(), elsewhere);

        match previous {
            Some(value) => unsafe {
                std::env::set_var(crate::runtime_paths::HARN_STATE_DIR_ENV, value);
            },
            None => unsafe { std::env::remove_var(crate::runtime_paths::HARN_STATE_DIR_ENV) },
        }
    }

    #[test]
    fn canonical_and_legacy_paths_are_explicitly_distinct() {
        let state_dir = SessionStoreDir::under_root(Path::new("/workspace"));
        assert_eq!(
            store_path(&state_dir),
            Path::new("/workspace/.harn/session-store.sqlite")
        );
        assert_ne!(
            store_path(&state_dir),
            legacy_session_path(&state_dir, "session").unwrap()
        );
    }

    #[test]
    fn malformed_option_metadata_is_rejected() {
        let options = json_to_vm_value(&json!({
            "root": 7,
            "tags": ["ok", 9],
            "headers": {"run_id": 3},
        }));
        let options = options.as_dict().unwrap();
        assert!(store_state_dir(Some(options)).is_err());
        assert!(option_tags(Some(options)).is_err());
        assert!(option_headers(Some(options)).is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn retired_timestamp_override_is_rejected_loudly() {
        let error = session_store_append_impl(
            crate::vm::AsyncBuiltinCtx::for_test(crate::vm::Vm::new()),
            vec![
                VmValue::String(arcstr::ArcStr::from("session")),
                VmValue::Nil,
                json_to_vm_value(&json!({"now": "2020-01-01T00:00:00Z"})),
            ],
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("no longer supported"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn legacy_import_is_idempotent_and_preserves_source_facts() {
        let dir = tempfile::tempdir().unwrap();
        let session_id = "legacy.session";
        let legacy_path =
            write_legacy_fixture(&SessionStoreDir::under_root(dir.path()), session_id);
        let legacy_before = fs::read_to_string(&legacy_path).unwrap();
        let store = open_store(&SessionStoreDir::under_root(dir.path())).unwrap();

        assert!(ensure_session(
            &store,
            &SessionStoreDir::under_root(dir.path()),
            session_id,
            false,
            None
        )
        .await
        .unwrap());
        let events = read_all_events(&store, session_id).await.unwrap();

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event_id, 1);
        assert_eq!(events[1].parent_event_id, Some(1));
        assert_eq!(events[0].headers["source_event_id"], "1");
        assert_eq!(
            events[0].headers["harn.legacy.record_hash"],
            serde_json::from_str::<JsonValue>(legacy_before.lines().next().unwrap()).unwrap()
                ["record_hash"]
                .as_str()
                .unwrap()
        );
        assert_eq!(events[0].headers["x-source"], "7");
        assert_eq!(events[0].headers["authorization"], "[redacted]");
        assert!(!events[0].headers["x-debug"].contains("sk-proj-"));
        let original_headers =
            serde_json::from_str::<JsonValue>(&events[0].headers["harn.legacy.original_headers"])
                .unwrap();
        assert_eq!(original_headers["run_id"], " ");
        assert_eq!(original_headers["authorization"], "[redacted]");
        assert_eq!(original_headers["harn.legacy.event_id"], "source-value");
        assert_eq!(
            original_headers["harn.legacy.header.run_id"],
            "source-relocation-value"
        );
        assert!(store.verify(session_id).await.unwrap().failures.is_empty());

        drop(store);
        let store = open_store(&SessionStoreDir::under_root(dir.path())).unwrap();
        assert!(ensure_session(
            &store,
            &SessionStoreDir::under_root(dir.path()),
            session_id,
            false,
            None
        )
        .await
        .unwrap());
        assert_eq!(
            store.describe(session_id).await.unwrap().event_count,
            events.len()
        );
        assert_eq!(fs::read_to_string(legacy_path).unwrap(), legacy_before);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn imported_legacy_source_does_not_resurrect_after_hard_delete() {
        let dir = tempfile::tempdir().unwrap();
        let session_id = "deleted.session";
        write_legacy_fixture(&SessionStoreDir::under_root(dir.path()), session_id);
        let store = open_store(&SessionStoreDir::under_root(dir.path())).unwrap();
        ensure_session(
            &store,
            &SessionStoreDir::under_root(dir.path()),
            session_id,
            false,
            None,
        )
        .await
        .unwrap();
        store.hard_delete(session_id).await.unwrap();
        drop(store);

        let store = open_store(&SessionStoreDir::under_root(dir.path())).unwrap();
        let error = ensure_session(
            &store,
            &SessionStoreDir::under_root(dir.path()),
            session_id,
            false,
            None,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("was already imported"));
        assert!(matches!(
            store.describe(session_id).await,
            Err(StoreError::NotFound(_))
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn canonical_collision_never_strands_a_legacy_source_silently() {
        let dir = tempfile::tempdir().unwrap();
        let session_id = "collision.session";
        let store = open_store(&SessionStoreDir::under_root(dir.path())).unwrap();
        store
            .create(CreateSession {
                id: Some(session_id.to_string()),
                ..CreateSession::default()
            })
            .await
            .unwrap();
        write_legacy_fixture(&SessionStoreDir::under_root(dir.path()), session_id);

        for _ in 0..2 {
            let error = ensure_session(
                &store,
                &SessionStoreDir::under_root(dir.path()),
                session_id,
                false,
                None,
            )
            .await
            .unwrap_err();
            assert!(error.to_string().contains("already exists"));
        }
        assert_eq!(store.describe(session_id).await.unwrap().event_count, 0);
    }

    #[test]
    fn corrupt_or_mixed_tenant_legacy_stream_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let session_id = "invalid.session";
        let path = write_legacy_fixture(&SessionStoreDir::under_root(dir.path()), session_id);
        let original = fs::read_to_string(&path).unwrap();
        let mut rows = original
            .lines()
            .map(|line| serde_json::from_str::<JsonValue>(line).unwrap())
            .collect::<Vec<_>>();

        rows[1]["payload"] = json!({"tampered": true});
        fs::write(
            &path,
            rows.iter()
                .map(JsonValue::to_string)
                .collect::<Vec<_>>()
                .join("\n")
                + "\n",
        )
        .unwrap();
        assert!(
            read_legacy_events(&SessionStoreDir::under_root(dir.path()), session_id)
                .unwrap_err()
                .to_string()
                .contains("record_hash mismatch")
        );

        let mut rows = original
            .lines()
            .map(|line| serde_json::from_str::<JsonValue>(line).unwrap())
            .collect::<Vec<_>>();
        rows[1]["tenant_id"] = json!("tenant-b");
        fs::write(
            &path,
            rows.iter()
                .map(JsonValue::to_string)
                .collect::<Vec<_>>()
                .join("\n")
                + "\n",
        )
        .unwrap();
        assert!(
            read_legacy_events(&SessionStoreDir::under_root(dir.path()), session_id)
                .unwrap_err()
                .to_string()
                .contains("mixed tenant_id")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn append_rejects_tenant_drift() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(&SessionStoreDir::under_root(dir.path())).unwrap();
        assert!(ensure_session(
            &store,
            &SessionStoreDir::under_root(dir.path()),
            "tenant.session",
            true,
            Some("tenant-a".to_string()),
        )
        .await
        .unwrap());

        let error = ensure_session(
            &store,
            &SessionStoreDir::under_root(dir.path()),
            "tenant.session",
            true,
            Some("tenant-b".to_string()),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("does not match"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn reads_and_verification_reject_tenant_drift() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(&SessionStoreDir::under_root(dir.path())).unwrap();
        assert!(ensure_session(
            &store,
            &SessionStoreDir::under_root(dir.path()),
            "tenant.session",
            true,
            Some("tenant-a".to_string()),
        )
        .await
        .unwrap());
        drop(store);
        let args = || {
            vec![
                VmValue::String(arcstr::ArcStr::from("tenant.session")),
                json_to_vm_value(&json!({
                    "root": dir.path().to_string_lossy(),
                    "tenant_id": "tenant-b",
                })),
            ]
        };

        let events_error = session_store_events_impl(
            crate::vm::AsyncBuiltinCtx::for_test(crate::vm::Vm::new()),
            args(),
        )
        .await
        .unwrap_err();
        assert!(events_error.to_string().contains("does not match"));

        let verify_error = session_store_verify_impl(
            crate::vm::AsyncBuiltinCtx::for_test(crate::vm::Vm::new()),
            args(),
        )
        .await
        .unwrap_err();
        assert!(verify_error.to_string().contains("does not match"));
    }

    async fn exercise_execution_env_store(
        label: &'static str,
        patch_mode: bool,
        root: PathBuf,
        fallback_root: PathBuf,
    ) -> PathBuf {
        crate::orchestration::scope_ambient(
            crate::orchestration::AmbientExecutionScope::default(),
            async move {
                let root_value = root.to_string_lossy().into_owned();
                let mut env = BTreeMap::new();
                if patch_mode {
                    env.insert("HARN_TEST_INHERITED".to_string(), "kept".to_string());
                }
                env.insert(STORE_ROOT_ENV.to_string(), root_value.clone());
                crate::stdlib::process::set_thread_execution_context(Some(
                    crate::orchestration::RunExecutionRecord {
                        cwd: Some(fallback_root.to_string_lossy().into_owned()),
                        project_root: Some(fallback_root.to_string_lossy().into_owned()),
                        env,
                        ..Default::default()
                    },
                ));

                tokio::task::yield_now().await;
                assert_eq!(
                    crate::stdlib::process::read_env_value(STORE_ROOT_ENV).as_deref(),
                    Some(root_value.as_str()),
                    "{label}: VM-visible environment must expose the child root"
                );
                assert_eq!(
                    canonical_store_state_dir(None).unwrap(),
                    SessionStoreDir::under_root(&root),
                    "{label}: native store root must match the VM-visible environment"
                );

                let ctx = crate::vm::AsyncBuiltinCtx::for_test(crate::vm::Vm::new());
                let database_path = session_store_database_path_impl(ctx.clone(), Vec::new())
                    .await
                    .unwrap();
                let database_path = PathBuf::from(database_path.display());
                assert_eq!(
                    database_path,
                    store_path(&SessionStoreDir::under_root(&root))
                );

                session_store_append_impl(
                    ctx.clone(),
                    vec![
                        VmValue::String(arcstr::ArcStr::from(format!("session.{label}"))),
                        json_to_vm_value(&json!({"child": label})),
                    ],
                )
                .await
                .unwrap();
                tokio::task::yield_now().await;

                let events = session_store_events_impl(
                    ctx,
                    vec![VmValue::String(arcstr::ArcStr::from(format!(
                        "session.{label}"
                    )))],
                )
                .await
                .unwrap();
                let VmValue::List(events) = events else {
                    panic!("{label}: expected a list of stored events");
                };
                assert_eq!(events.len(), 1, "{label}: expected one isolated event");
                let child = events[0]
                    .as_dict()
                    .and_then(|event| event.get("payload"))
                    .and_then(VmValue::as_dict)
                    .and_then(|payload| payload.get("child"))
                    .map(VmValue::display);
                assert_eq!(child.as_deref(), Some(label));
                database_path
            },
        )
        .await
    }

    #[tokio::test(flavor = "current_thread")]
    async fn execution_env_isolates_patch_replace_and_concurrent_stores() {
        let fallback = tempfile::tempdir().unwrap();
        let patch = tempfile::tempdir().unwrap();
        let replace = tempfile::tempdir().unwrap();
        let fallback_root = fallback.path().to_path_buf();
        let patch_root = patch.path().to_path_buf();
        let replace_root = replace.path().to_path_buf();

        let local = tokio::task::LocalSet::new();
        let (patch_path, replace_path) = local
            .run_until(async {
                let patch_child = tokio::task::spawn_local(exercise_execution_env_store(
                    "patch",
                    true,
                    patch_root.clone(),
                    fallback_root.clone(),
                ));
                let replace_child = tokio::task::spawn_local(exercise_execution_env_store(
                    "replace",
                    false,
                    replace_root.clone(),
                    fallback_root.clone(),
                ));
                (patch_child.await.unwrap(), replace_child.await.unwrap())
            })
            .await;

        assert_eq!(
            patch_path,
            store_path(&SessionStoreDir::under_root(&patch_root))
        );
        assert_eq!(
            replace_path,
            store_path(&SessionStoreDir::under_root(&replace_root))
        );
        assert!(patch_path.is_file());
        assert!(replace_path.is_file());
        assert_ne!(patch_path, replace_path);
        assert!(
            !store_path(&SessionStoreDir::under_root(&fallback_root)).exists(),
            "the inherited/global fallback store must remain untouched"
        );
    }
}
