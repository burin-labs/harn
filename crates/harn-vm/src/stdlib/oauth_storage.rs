//! `std/oauth/storage` — OAuth token storage backends.
//!
//! Implements the [OA-03 issue] storage abstraction with five backends:
//! `memory`, `file` (AES-256-GCM at rest), `harn_cloud_session`,
//! `harn_cloud_org`, and `custom` (vault integrations). Each backend
//! exposes the same `get / set / delete` surface; backend-specific
//! configuration is captured in an opaque handle dict produced by the
//! corresponding constructor.
//!
//! [OA-03 issue]: https://github.com/burin-labs/harn/issues/1904
//!
//! ## Backend dispatch
//!
//! `memory`, `file`, and `harn_cloud_*` are dispatched by name to the
//! Rust implementations below. `custom` is dispatched by the Harn-side
//! wrapper module which invokes user-supplied closures; this file never
//! sees `custom` handles. Cloud backends route through the
//! `oauth_storage` host capability (`cloud_get / cloud_set /
//! cloud_delete`) so embedders such as a cloud platform or IDE host can
//! supply tenant-scoped storage; without a bridge they raise a
//! deterministic "not configured" error.
//!
//! ## File backend wire format
//!
//! Tokens are serialized as JSON `{key: TokenSet}` and sealed with
//! AES-256-GCM. The on-disk envelope is JSON `{ version: 1, nonce:
//! base64, ciphertext: base64 }`. The 32-byte key is derived from the
//! caller-supplied secret via HKDF-SHA256 (`info =
//! "harn-oauth-storage-v1"`), so callers may pass any high-entropy
//! string or bytes value. A fresh random nonce is sampled on every
//! write, and atomic writes go through a sibling `.tmp` file plus
//! rename.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Key as AesKey, Nonce};
use base64::engine::general_purpose::STANDARD_NO_PAD;
use base64::Engine as _;
use hkdf::Hkdf;
use rand::Rng as _;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sha2::Sha256;

use crate::llm::vm_value_to_json;
use crate::stdlib::host::dispatch_host_operation;
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

const HANDLE_KEY_KIND: &str = "kind";
const HANDLE_KEY_ID: &str = "id";
const HANDLE_KEY_PATH: &str = "path";
const HANDLE_KEY_SCOPE: &str = "scope";

const KIND_MEMORY: &str = "memory";
const KIND_FILE: &str = "file";
const KIND_HARN_CLOUD_SESSION: &str = "harn_cloud_session";
const KIND_HARN_CLOUD_ORG: &str = "harn_cloud_org";

const HKDF_INFO: &[u8] = b"harn-oauth-storage-v1";
const FILE_ENVELOPE_VERSION: u32 = 1;

thread_local! {
    static MEMORY_STORE: RefCell<BTreeMap<String, BTreeMap<String, StoredEntry>>> =
        const { RefCell::new(std::collections::BTreeMap::new()) };

    /// File-handle encryption keys, kept off the user-visible handle
    /// dict so a stray `to_string(handle)` cannot exfiltrate the secret.
    static FILE_SECRETS: RefCell<BTreeMap<String, Vec<u8>>> =
        const { RefCell::new(std::collections::BTreeMap::new()) };
}

static MEMORY_ID_COUNTER: Mutex<u64> = Mutex::new(0);
static FILE_ID_COUNTER: Mutex<u64> = Mutex::new(0);

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredEntry {
    token: JsonValue,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ttl_seconds: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct FileEnvelope {
    version: u32,
    nonce: String,
    ciphertext: String,
}

pub(crate) fn register_oauth_storage_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

pub(crate) const MODULE_BUILTINS: &[&crate::stdlib::macros::VmBuiltinDef] = &[
    &OAUTH_STORAGE_MEMORY_HANDLE_IMPL_DEF,
    &OAUTH_STORAGE_FILE_HANDLE_IMPL_DEF,
    &OAUTH_STORAGE_CLOUD_HANDLE_IMPL_DEF,
    &OAUTH_STORAGE_GET_IMPL_DEF,
    &OAUTH_STORAGE_SET_IMPL_DEF,
    &OAUTH_STORAGE_DELETE_IMPL_DEF,
];

#[crate::stdlib::macros::harn_builtin(
    sig = "__oauth_storage_memory_handle() -> dict",
    category = "oauth_storage"
)]
fn oauth_storage_memory_handle_impl(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    if !args.is_empty() {
        return Err(VmError::Runtime(
            "__oauth_storage_memory_handle: expected 0 arguments".to_string(),
        ));
    }
    Ok(memory_handle())
}

#[crate::stdlib::macros::harn_builtin(
    sig = "__oauth_storage_file_handle(path: string, secret: any) -> dict",
    category = "oauth_storage"
)]
fn oauth_storage_file_handle_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let path = required_string_arg(args, 0, "__oauth_storage_file_handle", "path")?;
    let secret = required_bytes_or_string(args, 1, "__oauth_storage_file_handle")?;
    Ok(file_handle(&path, &secret))
}

#[crate::stdlib::macros::harn_builtin(
    sig = "__oauth_storage_cloud_handle(scope: string) -> dict",
    category = "oauth_storage"
)]
fn oauth_storage_cloud_handle_impl(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let scope = required_string_arg(args, 0, "__oauth_storage_cloud_handle", "scope")?;
    let kind = match scope.as_str() {
        "session" => KIND_HARN_CLOUD_SESSION,
        "org" => KIND_HARN_CLOUD_ORG,
        other => {
            return Err(VmError::Runtime(format!(
                "__oauth_storage_cloud_handle: scope must be \"session\" or \"org\", got `{other}`"
            )))
        }
    };
    Ok(cloud_handle(kind))
}

#[crate::stdlib::macros::harn_builtin(
    sig = "__oauth_storage_get(handle: dict, key: string) -> any",
    kind = "async",
    category = "oauth_storage"
)]
async fn oauth_storage_get_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let handle = require_handle(&args, 0, "__oauth_storage_get")?;
    let key = required_string_arg(&args, 1, "__oauth_storage_get", "key")?;
    backend_get(&handle, &key).await
}

#[crate::stdlib::macros::harn_builtin(
    sig = "__oauth_storage_set(handle: dict, key: string, token_set: dict, ttl_seconds?: int) -> nil",
    kind = "async",
    category = "oauth_storage"
)]
async fn oauth_storage_set_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let handle = require_handle(&args, 0, "__oauth_storage_set")?;
    let key = required_string_arg(&args, 1, "__oauth_storage_set", "key")?;
    let token_dict = require_dict_arg(&args, 2, "__oauth_storage_set", "token_set")?;
    let ttl = optional_int_arg(&args, 3, "__oauth_storage_set", "ttl_seconds")?;
    backend_set(&handle, &key, &token_dict, ttl).await?;
    Ok(VmValue::Nil)
}

#[crate::stdlib::macros::harn_builtin(
    sig = "__oauth_storage_delete(handle: dict, key: string) -> nil",
    kind = "async",
    category = "oauth_storage"
)]
async fn oauth_storage_delete_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let handle = require_handle(&args, 0, "__oauth_storage_delete")?;
    let key = required_string_arg(&args, 1, "__oauth_storage_delete", "key")?;
    backend_delete(&handle, &key).await?;
    Ok(VmValue::Nil)
}

fn memory_handle() -> VmValue {
    let id = {
        let mut guard = MEMORY_ID_COUNTER
            .lock()
            .expect("memory id counter poisoned");
        *guard = guard.wrapping_add(1);
        format!("memory-{}", *guard)
    };
    let mut fields = crate::value::DictMap::new();
    fields.insert(
        crate::value::intern_key(HANDLE_KEY_KIND),
        VmValue::string(KIND_MEMORY),
    );
    fields.insert(
        crate::value::intern_key(HANDLE_KEY_ID),
        VmValue::string(&id),
    );
    VmValue::dict(fields)
}

fn file_handle(path: &str, secret: &[u8]) -> VmValue {
    let counter = {
        let mut guard = FILE_ID_COUNTER.lock().expect("file id counter poisoned");
        *guard = guard.wrapping_add(1);
        *guard
    };
    let id = format!("file-{counter}");
    FILE_SECRETS.with(|secrets| {
        secrets.borrow_mut().insert(id.clone(), secret.to_vec());
    });
    let mut fields = crate::value::DictMap::new();
    fields.insert(
        crate::value::intern_key(HANDLE_KEY_KIND),
        VmValue::string(KIND_FILE),
    );
    fields.insert(
        crate::value::intern_key(HANDLE_KEY_ID),
        VmValue::string(&id),
    );
    fields.insert(
        crate::value::intern_key(HANDLE_KEY_PATH),
        VmValue::string(path),
    );
    VmValue::dict(fields)
}

fn cloud_handle(kind: &'static str) -> VmValue {
    let scope = if kind == KIND_HARN_CLOUD_ORG {
        "org"
    } else {
        "session"
    };
    let mut fields = crate::value::DictMap::new();
    fields.insert(
        crate::value::intern_key(HANDLE_KEY_KIND),
        VmValue::string(kind),
    );
    fields.insert(
        crate::value::intern_key(HANDLE_KEY_ID),
        VmValue::string(kind),
    );
    fields.insert(
        crate::value::intern_key(HANDLE_KEY_SCOPE),
        VmValue::string(scope),
    );
    VmValue::dict(fields)
}

fn handle_kind(handle: &crate::value::DictMap) -> Result<String, VmError> {
    handle
        .get(HANDLE_KEY_KIND)
        .and_then(|value| match value {
            VmValue::String(s) => Some(s.to_string()),
            _ => None,
        })
        .ok_or_else(|| {
            VmError::Runtime(
                "oauth storage handle is missing `kind` (was the handle built by `oauth/storage`?)"
                    .to_string(),
            )
        })
}

fn handle_id(handle: &crate::value::DictMap) -> Result<String, VmError> {
    handle
        .get(HANDLE_KEY_ID)
        .and_then(|value| match value {
            VmValue::String(s) => Some(s.to_string()),
            _ => None,
        })
        .ok_or_else(|| VmError::Runtime("oauth storage handle is missing `id`".to_string()))
}

async fn backend_get(handle: &crate::value::DictMap, key: &str) -> Result<VmValue, VmError> {
    match handle_kind(handle)?.as_str() {
        KIND_MEMORY => Ok(memory_get(&handle_id(handle)?, key)),
        KIND_FILE => file_get(handle, key),
        KIND_HARN_CLOUD_SESSION | KIND_HARN_CLOUD_ORG => cloud_get(handle, key).await,
        other => unsupported_kind(other),
    }
}

async fn backend_set(
    handle: &crate::value::DictMap,
    key: &str,
    token: &crate::value::DictMap,
    ttl_seconds: Option<i64>,
) -> Result<(), VmError> {
    let json_token = vm_value_to_json(&VmValue::dict(token.clone()));
    match handle_kind(handle)?.as_str() {
        KIND_MEMORY => {
            memory_set(&handle_id(handle)?, key, json_token, ttl_seconds);
            Ok(())
        }
        KIND_FILE => file_set(handle, key, json_token, ttl_seconds),
        KIND_HARN_CLOUD_SESSION | KIND_HARN_CLOUD_ORG => {
            cloud_set(handle, key, json_token, ttl_seconds).await
        }
        other => {
            unsupported_kind::<()>(other)?;
            Ok(())
        }
    }
}

async fn backend_delete(handle: &crate::value::DictMap, key: &str) -> Result<(), VmError> {
    match handle_kind(handle)?.as_str() {
        KIND_MEMORY => {
            memory_delete(&handle_id(handle)?, key);
            Ok(())
        }
        KIND_FILE => file_delete(handle, key),
        KIND_HARN_CLOUD_SESSION | KIND_HARN_CLOUD_ORG => cloud_delete(handle, key).await,
        other => {
            unsupported_kind::<()>(other)?;
            Ok(())
        }
    }
}

fn unsupported_kind<T>(kind: &str) -> Result<T, VmError> {
    Err(VmError::Runtime(format!(
        "oauth storage: unsupported backend kind `{kind}`. Use `OAuth.Storage.custom` for user-supplied storage; built-in kinds are `{KIND_MEMORY}`, `{KIND_FILE}`, `{KIND_HARN_CLOUD_SESSION}`, `{KIND_HARN_CLOUD_ORG}`."
    )))
}

fn memory_get(handle_id: &str, key: &str) -> VmValue {
    MEMORY_STORE.with(|store| {
        let store = store.borrow();
        match store.get(handle_id).and_then(|entries| entries.get(key)) {
            Some(entry) => json_to_vm_dict(&entry.token),
            None => VmValue::Nil,
        }
    })
}

fn memory_set(handle_id: &str, key: &str, token: JsonValue, ttl_seconds: Option<i64>) {
    MEMORY_STORE.with(|store| {
        let mut store = store.borrow_mut();
        let entry = store.entry(handle_id.to_string()).or_default();
        entry.insert(key.to_string(), StoredEntry { token, ttl_seconds });
    });
}

fn memory_delete(handle_id: &str, key: &str) {
    MEMORY_STORE.with(|store| {
        let mut store = store.borrow_mut();
        if let Some(entries) = store.get_mut(handle_id) {
            entries.remove(key);
            if entries.is_empty() {
                store.remove(handle_id);
            }
        }
    });
}

fn file_path(handle: &crate::value::DictMap) -> Result<PathBuf, VmError> {
    handle
        .get(HANDLE_KEY_PATH)
        .and_then(|value| match value {
            VmValue::String(s) => Some(PathBuf::from(s.as_str())),
            _ => None,
        })
        .ok_or_else(|| VmError::Runtime("oauth storage file handle is missing `path`".to_string()))
}

fn file_secret(handle: &crate::value::DictMap) -> Result<Vec<u8>, VmError> {
    let id = handle_id(handle)?;
    FILE_SECRETS
        .with(|secrets| secrets.borrow().get(&id).cloned())
        .ok_or_else(|| {
            VmError::Runtime(format!(
                "oauth storage: encryption key for file handle `{id}` is no longer registered"
            ))
        })
}

fn derive_file_key(secret: &[u8]) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(None, secret);
    let mut out = [0u8; 32];
    hk.expand(HKDF_INFO, &mut out)
        .expect("HKDF expand for 32 bytes never fails");
    out
}

fn read_file_entries(path: &Path, secret: &[u8]) -> Result<BTreeMap<String, StoredEntry>, VmError> {
    let raw = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(std::collections::BTreeMap::new());
        }
        Err(error) => {
            return Err(VmError::Runtime(format!(
                "oauth storage: failed to read `{}`: {error}",
                path.display()
            )));
        }
    };
    if raw.is_empty() {
        return Ok(std::collections::BTreeMap::new());
    }
    let envelope: FileEnvelope = serde_json::from_slice(&raw).map_err(|error| {
        VmError::Runtime(format!(
            "oauth storage: file `{}` is not a valid envelope: {error}",
            path.display()
        ))
    })?;
    if envelope.version != FILE_ENVELOPE_VERSION {
        return Err(VmError::Runtime(format!(
            "oauth storage: file `{}` envelope version {} not supported",
            path.display(),
            envelope.version
        )));
    }
    let nonce_bytes = STANDARD_NO_PAD
        .decode(envelope.nonce.as_bytes())
        .map_err(|error| {
            VmError::Runtime(format!("oauth storage: invalid envelope nonce: {error}"))
        })?;
    if nonce_bytes.len() != 12 {
        return Err(VmError::Runtime(format!(
            "oauth storage: envelope nonce must be 12 bytes, got {}",
            nonce_bytes.len()
        )));
    }
    let ciphertext = STANDARD_NO_PAD
        .decode(envelope.ciphertext.as_bytes())
        .map_err(|error| {
            VmError::Runtime(format!(
                "oauth storage: invalid envelope ciphertext: {error}"
            ))
        })?;
    let key = derive_file_key(secret);
    let cipher = Aes256Gcm::new(AesKey::<Aes256Gcm>::from_slice(&key));
    let nonce = Nonce::from_slice(&nonce_bytes);
    let plaintext = cipher
        .decrypt(
            nonce,
            Payload {
                msg: &ciphertext,
                aad: FILE_AAD,
            },
        )
        .map_err(|_| {
            VmError::Runtime(
                "oauth storage: decryption failed; encryption_key does not match the file"
                    .to_string(),
            )
        })?;
    let map: BTreeMap<String, StoredEntry> =
        serde_json::from_slice(&plaintext).map_err(|error| {
            VmError::Runtime(format!(
                "oauth storage: failed to decode decrypted payload: {error}"
            ))
        })?;
    Ok(map)
}

fn write_file_entries(
    path: &Path,
    secret: &[u8],
    entries: &BTreeMap<String, StoredEntry>,
) -> Result<(), VmError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|error| {
                VmError::Runtime(format!(
                    "oauth storage: failed to create parent directory `{}`: {error}",
                    parent.display()
                ))
            })?;
        }
    }
    let plaintext = serde_json::to_vec(entries).map_err(|error| {
        VmError::Runtime(format!("oauth storage: failed to encode entries: {error}"))
    })?;
    let key = derive_file_key(secret);
    let cipher = Aes256Gcm::new(AesKey::<Aes256Gcm>::from_slice(&key));
    let mut nonce_bytes = [0u8; 12];
    rand::rng().fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(
            nonce,
            Payload {
                msg: &plaintext,
                aad: FILE_AAD,
            },
        )
        .map_err(|_| VmError::Runtime("oauth storage: AES-GCM encryption failed".to_string()))?;
    let envelope = FileEnvelope {
        version: FILE_ENVELOPE_VERSION,
        nonce: STANDARD_NO_PAD.encode(nonce_bytes),
        ciphertext: STANDARD_NO_PAD.encode(&ciphertext),
    };
    let encoded = serde_json::to_vec(&envelope).map_err(|error| {
        VmError::Runtime(format!("oauth storage: failed to encode envelope: {error}"))
    })?;
    let tmp_path = match path.file_name() {
        Some(name) => {
            let mut buf = path.to_path_buf();
            let mut filename = name.to_os_string();
            filename.push(".tmp");
            buf.set_file_name(filename);
            buf
        }
        None => {
            return Err(VmError::Runtime(format!(
                "oauth storage: invalid file path `{}`",
                path.display()
            )));
        }
    };
    {
        let mut file = fs::File::create(&tmp_path).map_err(|error| {
            VmError::Runtime(format!(
                "oauth storage: failed to open `{}`: {error}",
                tmp_path.display()
            ))
        })?;
        file.write_all(&encoded).map_err(|error| {
            VmError::Runtime(format!(
                "oauth storage: failed to write `{}`: {error}",
                tmp_path.display()
            ))
        })?;
        file.sync_all().map_err(|error| {
            VmError::Runtime(format!(
                "oauth storage: failed to sync `{}`: {error}",
                tmp_path.display()
            ))
        })?;
        // The envelope is AES-GCM sealed, but the on-disk file should still be
        // owner-only so a wide umask (0644) can't leave it group/world-readable.
        // Set the mode on the temp file before the rename so the final path is
        // never observable at looser permissions.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(0o600))
                .map_err(|error| {
                    VmError::Runtime(format!(
                        "oauth storage: failed to set permissions on `{}`: {error}",
                        tmp_path.display()
                    ))
                })?;
        }
    }
    fs::rename(&tmp_path, path).map_err(|error| {
        VmError::Runtime(format!(
            "oauth storage: failed to rename `{}` to `{}`: {error}",
            tmp_path.display(),
            path.display()
        ))
    })
}

/// AAD label binds the ciphertext to this storage format. Callers who
/// relocate the file can still decrypt with the same key; tying AAD to
/// the path would only break that legitimate migration without
/// meaningfully constraining an attacker who can already move files.
const FILE_AAD: &[u8] = HKDF_INFO;

fn file_get(handle: &crate::value::DictMap, key: &str) -> Result<VmValue, VmError> {
    let path = file_path(handle)?;
    let secret = file_secret(handle)?;
    let entries = read_file_entries(&path, &secret)?;
    Ok(match entries.get(key) {
        Some(entry) => json_to_vm_dict(&entry.token),
        None => VmValue::Nil,
    })
}

fn file_set(
    handle: &crate::value::DictMap,
    key: &str,
    token: JsonValue,
    ttl_seconds: Option<i64>,
) -> Result<(), VmError> {
    let path = file_path(handle)?;
    let secret = file_secret(handle)?;
    let mut entries = read_file_entries(&path, &secret)?;
    entries.insert(key.to_string(), StoredEntry { token, ttl_seconds });
    write_file_entries(&path, &secret, &entries)
}

fn file_delete(handle: &crate::value::DictMap, key: &str) -> Result<(), VmError> {
    let path = file_path(handle)?;
    let secret = file_secret(handle)?;
    let mut entries = read_file_entries(&path, &secret)?;
    if entries.remove(key).is_none() {
        return Ok(());
    }
    if entries.is_empty() {
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(VmError::Runtime(format!(
                "oauth storage: failed to remove `{}`: {error}",
                path.display()
            ))),
        }
    } else {
        write_file_entries(&path, &secret, &entries)
    }
}

fn cloud_scope(handle: &crate::value::DictMap) -> Result<String, VmError> {
    handle
        .get(HANDLE_KEY_SCOPE)
        .and_then(|value| match value {
            VmValue::String(s) => Some(s.to_string()),
            _ => None,
        })
        .ok_or_else(|| {
            VmError::Runtime("oauth storage cloud handle is missing `scope`".to_string())
        })
}

async fn cloud_get(handle: &crate::value::DictMap, key: &str) -> Result<VmValue, VmError> {
    let scope = cloud_scope(handle)?;
    let mut params = crate::value::DictMap::new();
    params.insert(crate::value::intern_key("scope"), VmValue::string(&scope));
    params.insert(crate::value::intern_key("key"), VmValue::string(key));
    dispatch_host_operation("oauth_storage", "cloud_get", &params).await
}

async fn cloud_set(
    handle: &crate::value::DictMap,
    key: &str,
    token: JsonValue,
    ttl_seconds: Option<i64>,
) -> Result<(), VmError> {
    let scope = cloud_scope(handle)?;
    let mut params = crate::value::DictMap::new();
    params.insert(crate::value::intern_key("scope"), VmValue::string(&scope));
    params.insert(crate::value::intern_key("key"), VmValue::string(key));
    params.insert(crate::value::intern_key("token"), json_to_vm_dict(&token));
    if let Some(ttl) = ttl_seconds {
        params.insert(crate::value::intern_key("ttl_seconds"), VmValue::Int(ttl));
    }
    dispatch_host_operation("oauth_storage", "cloud_set", &params).await?;
    Ok(())
}

async fn cloud_delete(handle: &crate::value::DictMap, key: &str) -> Result<(), VmError> {
    let scope = cloud_scope(handle)?;
    let mut params = crate::value::DictMap::new();
    params.insert(crate::value::intern_key("scope"), VmValue::string(&scope));
    params.insert(crate::value::intern_key("key"), VmValue::string(key));
    dispatch_host_operation("oauth_storage", "cloud_delete", &params).await?;
    Ok(())
}

fn json_to_vm_dict(value: &JsonValue) -> VmValue {
    crate::schema::json_to_vm_value(value)
}

fn require_handle(
    args: &[VmValue],
    index: usize,
    fn_name: &str,
) -> Result<crate::value::DictMap, VmError> {
    match args.get(index) {
        Some(VmValue::Dict(dict)) => Ok(dict.as_ref().clone()),
        Some(other) => Err(VmError::Runtime(format!(
            "{fn_name}: handle argument must be a dict, got {}",
            other.type_name()
        ))),
        None => Err(VmError::Runtime(format!(
            "{fn_name}: missing handle argument"
        ))),
    }
}

fn required_string_arg(
    args: &[VmValue],
    index: usize,
    fn_name: &str,
    arg_name: &str,
) -> Result<String, VmError> {
    match args.get(index) {
        Some(VmValue::String(s)) => Ok(s.to_string()),
        Some(other) => Err(VmError::Runtime(format!(
            "{fn_name}: `{arg_name}` must be a string, got {}",
            other.type_name()
        ))),
        None => Err(VmError::Runtime(format!(
            "{fn_name}: `{arg_name}` argument is required"
        ))),
    }
}

fn required_bytes_or_string(
    args: &[VmValue],
    index: usize,
    fn_name: &str,
) -> Result<Vec<u8>, VmError> {
    match args.get(index) {
        Some(VmValue::Bytes(bytes)) => {
            if bytes.is_empty() {
                Err(VmError::Runtime(format!(
                    "{fn_name}: `encryption_key` must not be empty"
                )))
            } else {
                Ok(bytes.as_ref().clone())
            }
        }
        Some(VmValue::String(s)) => {
            if s.is_empty() {
                Err(VmError::Runtime(format!(
                    "{fn_name}: `encryption_key` must not be empty"
                )))
            } else {
                Ok(s.as_bytes().to_vec())
            }
        }
        Some(other) => Err(VmError::Runtime(format!(
            "{fn_name}: `encryption_key` must be bytes or string, got {}",
            other.type_name()
        ))),
        None => Err(VmError::Runtime(format!(
            "{fn_name}: `encryption_key` argument is required"
        ))),
    }
}

fn require_dict_arg(
    args: &[VmValue],
    index: usize,
    fn_name: &str,
    arg_name: &str,
) -> Result<crate::value::DictMap, VmError> {
    match args.get(index) {
        Some(VmValue::Dict(dict)) => Ok(dict.as_ref().clone()),
        Some(other) => Err(VmError::Runtime(format!(
            "{fn_name}: `{arg_name}` must be a dict, got {}",
            other.type_name()
        ))),
        None => Err(VmError::Runtime(format!(
            "{fn_name}: `{arg_name}` argument is required"
        ))),
    }
}

fn optional_int_arg(
    args: &[VmValue],
    index: usize,
    fn_name: &str,
    arg_name: &str,
) -> Result<Option<i64>, VmError> {
    match args.get(index) {
        Some(VmValue::Nil) | None => Ok(None),
        Some(VmValue::Int(value)) => Ok(Some(*value)),
        Some(VmValue::Duration(value)) => Ok(Some(*value / 1000)),
        Some(other) => Err(VmError::Runtime(format!(
            "{fn_name}: `{arg_name}` must be int or duration, got {}",
            other.type_name()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::values_equal;

    fn token_dict(access: &str) -> crate::value::DictMap {
        let mut dict = crate::value::DictMap::new();
        dict.insert(
            crate::value::intern_key("access_token"),
            VmValue::string(access),
        );
        dict
    }

    fn dict_from_vm(value: &VmValue) -> crate::value::DictMap {
        match value {
            VmValue::Dict(dict) => dict.as_ref().clone(),
            other => panic!("expected dict, got {}", other.type_name()),
        }
    }

    fn assert_access_token(value: &VmValue, expected: &str) {
        let dict = dict_from_vm(value);
        let access = dict
            .get("access_token")
            .unwrap_or_else(|| panic!("missing access_token field"));
        assert!(
            values_equal(access, &VmValue::string(expected)),
            "expected access_token=`{expected}`, got {access:?}"
        );
    }

    #[tokio::test]
    async fn memory_backend_roundtrips() {
        let handle = match memory_handle() {
            VmValue::Dict(dict) => dict.as_ref().clone(),
            other => panic!("expected dict handle, got {other:?}"),
        };
        backend_set(&handle, "primary", &token_dict("hello"), Some(3600))
            .await
            .unwrap();
        let fetched = backend_get(&handle, "primary").await.unwrap();
        assert_access_token(&fetched, "hello");
        backend_delete(&handle, "primary").await.unwrap();
        let after = backend_get(&handle, "primary").await.unwrap();
        assert!(matches!(after, VmValue::Nil));
    }

    #[tokio::test]
    async fn file_backend_encrypts_at_rest() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("tokens.bin");
        let handle = match file_handle(path.to_str().unwrap(), b"correct-horse-battery-staple") {
            VmValue::Dict(dict) => dict.as_ref().clone(),
            other => panic!("expected dict handle, got {other:?}"),
        };
        backend_set(&handle, "k", &token_dict("super-secret"), None)
            .await
            .unwrap();
        let raw = std::fs::read(&path).unwrap();
        let envelope: FileEnvelope = serde_json::from_slice(&raw).unwrap();
        assert_eq!(envelope.version, FILE_ENVELOPE_VERSION);
        assert!(!String::from_utf8_lossy(&raw).contains("super-secret"));
        let fetched = backend_get(&handle, "k").await.unwrap();
        assert_access_token(&fetched, "super-secret");
        backend_delete(&handle, "k").await.unwrap();
        let after = backend_get(&handle, "k").await.unwrap();
        assert!(matches!(after, VmValue::Nil));
        assert!(!path.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn file_backend_writes_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("tokens.bin");
        let handle = match file_handle(path.to_str().unwrap(), b"correct-horse-battery-staple") {
            VmValue::Dict(dict) => dict.as_ref().clone(),
            other => panic!("expected dict handle, got {other:?}"),
        };
        backend_set(&handle, "k", &token_dict("super-secret"), None)
            .await
            .unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "expected token file mode 0o600, got {:o}",
            mode & 0o777
        );
    }

    #[tokio::test]
    async fn file_backend_rejects_wrong_key() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("tokens.bin");
        let good = match file_handle(path.to_str().unwrap(), b"good-secret") {
            VmValue::Dict(dict) => dict.as_ref().clone(),
            other => panic!("expected dict, got {other:?}"),
        };
        backend_set(&good, "k", &token_dict("payload"), None)
            .await
            .unwrap();
        let bad = match file_handle(path.to_str().unwrap(), b"bad-secret") {
            VmValue::Dict(dict) => dict.as_ref().clone(),
            other => panic!("expected dict, got {other:?}"),
        };
        let result = backend_get(&bad, "k").await;
        assert!(result.is_err(), "expected decryption error, got {result:?}");
    }
}
