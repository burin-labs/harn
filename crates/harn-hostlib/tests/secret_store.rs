//! Integration tests for the secret-store hostlib capability.
//!
//! These tests run with `HARN_SECRET_STORE_BACKEND=file` and
//! `HARN_SECRET_STORE_FILE_ROOT` redirected to a per-test `TempDir`, so
//! they pass on every CI runner regardless of whether a real Keychain or
//! Credential Manager is reachable. The OS-native backends are exercised
//! by `os_native.rs` on the matching target.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::Mutex;

use harn_hostlib::{
    secret_store::SecretStoreCapability, BuiltinRegistry, HostlibCapability, HostlibError,
};
use harn_vm::VmValue;
use tempfile::TempDir;

/// Process-wide lock guarding `HARN_SECRET_STORE_BACKEND` mutation. Cargo
/// runs tests in parallel by default, and the env var is read on every
/// builtin call, so we need to serialize writers.
fn env_lock() -> &'static Mutex<()> {
    static LOCK: Mutex<()> = Mutex::new(());
    &LOCK
}

struct EnvGuard {
    keys: Vec<&'static str>,
    prior: Vec<Option<String>>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl EnvGuard {
    fn new(vars: &[(&'static str, &str)]) -> Self {
        let lock = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let mut keys = Vec::with_capacity(vars.len());
        let mut prior = Vec::with_capacity(vars.len());
        for (key, value) in vars {
            keys.push(*key);
            prior.push(std::env::var(key).ok());
            unsafe {
                std::env::set_var(key, value);
            }
        }
        Self {
            keys,
            prior,
            _lock: lock,
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, prior) in self.keys.iter().zip(self.prior.iter()) {
            unsafe {
                match prior {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
        }
    }
}

fn registry() -> BuiltinRegistry {
    let mut registry = BuiltinRegistry::new();
    SecretStoreCapability.register_builtins(&mut registry);
    registry
}

fn dict_arg(entries: &[(&str, VmValue)]) -> Vec<VmValue> {
    let mut map: harn_vm::value::DictMap = Default::default();
    for (k, v) in entries {
        map.insert(k.to_string(), v.clone());
    }
    vec![VmValue::dict(map)]
}

fn vm_string(s: &str) -> VmValue {
    VmValue::String(Arc::from(s))
}

fn dict_get<'a>(value: &'a VmValue, key: &str) -> &'a VmValue {
    match value {
        VmValue::Dict(d) => d.get(key).expect("key present"),
        other => panic!("not a dict: {other:?}"),
    }
}

fn as_string(value: &VmValue) -> &str {
    match value {
        VmValue::String(s) => s.as_ref(),
        other => panic!("not a string: {other:?}"),
    }
}

#[test]
fn round_trip_set_get_delete_via_file_backend() {
    let root = TempDir::new().unwrap();
    let _env = EnvGuard::new(&[
        ("HARN_SECRET_STORE_BACKEND", "file"),
        ("HARN_SECRET_STORE_FILE_ROOT", root.path().to_str().unwrap()),
    ]);
    let reg = registry();

    let set = reg.find("hostlib_secret_store_set").unwrap();
    let get = reg.find("hostlib_secret_store_get").unwrap();
    let delete = reg.find("hostlib_secret_store_delete").unwrap();

    let set_args = dict_arg(&[
        ("account", vm_string("burin")),
        ("key", vm_string("OPENAI_API_KEY")),
        ("value", vm_string("sk-test")),
    ]);
    let resp = (set.handler)(&set_args).unwrap();
    assert_eq!(as_string(dict_get(&resp, "backend")), "file");

    let get_args = dict_arg(&[
        ("account", vm_string("burin")),
        ("key", vm_string("OPENAI_API_KEY")),
    ]);
    let resp = (get.handler)(&get_args).unwrap();
    assert_eq!(as_string(dict_get(&resp, "value")), "sk-test");

    let delete_resp = (delete.handler)(&get_args).unwrap();
    assert!(matches!(
        dict_get(&delete_resp, "deleted"),
        VmValue::Bool(true)
    ));

    let resp = (get.handler)(&get_args).unwrap();
    assert!(matches!(dict_get(&resp, "value"), VmValue::Nil));
}

#[test]
fn missing_value_returns_nil_with_backend_metadata() {
    let root = TempDir::new().unwrap();
    let _env = EnvGuard::new(&[
        ("HARN_SECRET_STORE_BACKEND", "file"),
        ("HARN_SECRET_STORE_FILE_ROOT", root.path().to_str().unwrap()),
    ]);
    let reg = registry();
    let get = reg.find("hostlib_secret_store_get").unwrap();
    let resp = (get.handler)(&dict_arg(&[
        ("account", vm_string("never-existed")),
        ("key", vm_string("nope")),
    ]))
    .unwrap();
    assert!(matches!(dict_get(&resp, "value"), VmValue::Nil));
    assert_eq!(as_string(dict_get(&resp, "backend")), "file");
}

#[test]
fn delete_returns_false_when_key_absent() {
    let root = TempDir::new().unwrap();
    let _env = EnvGuard::new(&[
        ("HARN_SECRET_STORE_BACKEND", "file"),
        ("HARN_SECRET_STORE_FILE_ROOT", root.path().to_str().unwrap()),
    ]);
    let reg = registry();
    let delete = reg.find("hostlib_secret_store_delete").unwrap();
    let resp = (delete.handler)(&dict_arg(&[
        ("account", vm_string("ghost")),
        ("key", vm_string("none")),
    ]))
    .unwrap();
    assert!(matches!(dict_get(&resp, "deleted"), VmValue::Bool(false)));
}

#[test]
fn list_returns_sorted_keys_for_account() {
    let root = TempDir::new().unwrap();
    let _env = EnvGuard::new(&[
        ("HARN_SECRET_STORE_BACKEND", "file"),
        ("HARN_SECRET_STORE_FILE_ROOT", root.path().to_str().unwrap()),
    ]);
    let reg = registry();
    let set = reg.find("hostlib_secret_store_set").unwrap();
    let list = reg.find("hostlib_secret_store_list").unwrap();

    for key in ["GAMMA", "ALPHA", "BETA"] {
        let _ = (set.handler)(&dict_arg(&[
            ("account", vm_string("app")),
            ("key", vm_string(key)),
            ("value", vm_string("v")),
        ]))
        .unwrap();
    }

    let resp = (list.handler)(&dict_arg(&[("account", vm_string("app"))])).unwrap();
    let keys = match dict_get(&resp, "keys") {
        VmValue::List(items) => items
            .iter()
            .map(|v| as_string(v).to_string())
            .collect::<Vec<_>>(),
        other => panic!("expected list, got {other:?}"),
    };
    assert_eq!(keys, vec!["ALPHA", "BETA", "GAMMA"]);
}

#[test]
fn accounts_are_namespaced_so_keys_do_not_collide() {
    let root = TempDir::new().unwrap();
    let _env = EnvGuard::new(&[
        ("HARN_SECRET_STORE_BACKEND", "file"),
        ("HARN_SECRET_STORE_FILE_ROOT", root.path().to_str().unwrap()),
    ]);
    let reg = registry();
    let set = reg.find("hostlib_secret_store_set").unwrap();
    let get = reg.find("hostlib_secret_store_get").unwrap();

    (set.handler)(&dict_arg(&[
        ("account", vm_string("alpha")),
        ("key", vm_string("TOKEN")),
        ("value", vm_string("alpha-token")),
    ]))
    .unwrap();
    (set.handler)(&dict_arg(&[
        ("account", vm_string("beta")),
        ("key", vm_string("TOKEN")),
        ("value", vm_string("beta-token")),
    ]))
    .unwrap();

    let alpha = (get.handler)(&dict_arg(&[
        ("account", vm_string("alpha")),
        ("key", vm_string("TOKEN")),
    ]))
    .unwrap();
    let beta = (get.handler)(&dict_arg(&[
        ("account", vm_string("beta")),
        ("key", vm_string("TOKEN")),
    ]))
    .unwrap();
    assert_eq!(as_string(dict_get(&alpha, "value")), "alpha-token");
    assert_eq!(as_string(dict_get(&beta, "value")), "beta-token");
}

#[test]
fn file_backend_writes_credentials_file_under_account_directory() {
    let root = TempDir::new().unwrap();
    let _env = EnvGuard::new(&[
        ("HARN_SECRET_STORE_BACKEND", "file"),
        ("HARN_SECRET_STORE_FILE_ROOT", root.path().to_str().unwrap()),
    ]);
    let reg = registry();
    let set = reg.find("hostlib_secret_store_set").unwrap();
    (set.handler)(&dict_arg(&[
        ("account", vm_string("burin")),
        ("key", vm_string("OPENAI_API_KEY")),
        ("value", vm_string("sk-test")),
    ]))
    .unwrap();

    let credentials = root.path().join("burin").join("credentials.json");
    assert!(credentials.exists(), "credentials file should exist");
    let body = std::fs::read_to_string(&credentials).unwrap();
    let parsed: BTreeMap<String, String> = serde_json::from_str(&body).unwrap();
    assert_eq!(
        parsed.get("OPENAI_API_KEY").map(String::as_str),
        Some("sk-test")
    );
}

#[cfg(unix)]
#[test]
fn file_backend_writes_credentials_with_owner_only_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let root = TempDir::new().unwrap();
    let _env = EnvGuard::new(&[
        ("HARN_SECRET_STORE_BACKEND", "file"),
        ("HARN_SECRET_STORE_FILE_ROOT", root.path().to_str().unwrap()),
    ]);
    let reg = registry();
    let set = reg.find("hostlib_secret_store_set").unwrap();
    (set.handler)(&dict_arg(&[
        ("account", vm_string("burin")),
        ("key", vm_string("OPENAI_API_KEY")),
        ("value", vm_string("sk-test")),
    ]))
    .unwrap();

    let credentials = root.path().join("burin").join("credentials.json");
    let mode = std::fs::metadata(&credentials)
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        mode, 0o600,
        "credentials file must be owner-read/write only"
    );
}

#[test]
fn empty_account_is_rejected_with_invalid_parameter() {
    let _env = EnvGuard::new(&[("HARN_SECRET_STORE_BACKEND", "file")]);
    let reg = registry();
    let get = reg.find("hostlib_secret_store_get").unwrap();
    let err = (get.handler)(&dict_arg(&[
        ("account", vm_string("")),
        ("key", vm_string("k")),
    ]))
    .unwrap_err();
    match err {
        HostlibError::InvalidParameter { param, .. } => {
            assert_eq!(param, "account");
        }
        other => panic!("expected InvalidParameter, got {other:?}"),
    }
}
