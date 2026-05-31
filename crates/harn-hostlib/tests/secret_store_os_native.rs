//! OS-native secret-store smoke tests.
//!
//! Exercises the live Keychain (macOS/iOS) and Credential Manager (Windows)
//! backends end-to-end. These tests:
//!
//! * Are skipped when `HARN_SECRET_STORE_BACKEND=file` is set in the
//!   environment — CI matrices that explicitly opt into the file backend
//!   should not trigger keychain prompts.
//! * Use a unique account name per run so concurrent CI invocations and
//!   leftover entries from prior failed runs cannot cause cross-contamination.
//! * Always clean up after themselves, even on assertion failure.
//!
//! On Linux there is no OS-native backend yet (secret-service is tracked
//! as a follow-on; see `docs/src/hostlib/secret_store.md`), so this file
//! compiles down to nothing on Linux targets.

#![cfg(any(target_os = "macos", target_os = "windows"))]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use harn_hostlib::{secret_store::SecretStoreCapability, BuiltinRegistry, HostlibCapability};
use harn_vm::VmValue;

fn registry() -> BuiltinRegistry {
    let mut registry = BuiltinRegistry::new();
    SecretStoreCapability.register_builtins(&mut registry);
    registry
}

fn dict_arg(entries: &[(&str, VmValue)]) -> Vec<VmValue> {
    let mut map: BTreeMap<String, VmValue> = BTreeMap::new();
    for (k, v) in entries {
        map.insert(k.to_string(), v.clone());
    }
    vec![VmValue::Dict(Arc::new(map))]
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

/// Process-unique account name so parallel test invocations cannot
/// collide. The pid plus an atomic counter is sufficient: PIDs differ
/// across concurrent processes, the counter differs across calls within
/// a process, and every test cleans up after itself via `scopeguard_delete`
/// (or the inline `Cleanup` RAII helper).
fn unique_account(prefix: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let pid = std::process::id();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("harn-hostlib-tests-{prefix}-{pid}-{n}")
}

fn skip_if_file_backend_forced() -> bool {
    matches!(
        std::env::var("HARN_SECRET_STORE_BACKEND").as_deref(),
        Ok("file")
    )
}

#[test]
fn os_native_round_trip_get_set_delete_list() {
    if skip_if_file_backend_forced() {
        eprintln!("skipping: HARN_SECRET_STORE_BACKEND=file is set");
        return;
    }
    let reg = registry();
    let account = unique_account("round-trip");
    let key1 = "primary";
    let key2 = "secondary";
    let set = reg.find("hostlib_secret_store_set").unwrap();
    let get = reg.find("hostlib_secret_store_get").unwrap();
    let list = reg.find("hostlib_secret_store_list").unwrap();
    let delete = reg.find("hostlib_secret_store_delete").unwrap();

    // Always clean up, even if an assertion below fires.
    struct Cleanup<'a> {
        delete: &'a harn_hostlib::RegisteredBuiltin,
        account: String,
        keys: Vec<&'static str>,
    }
    impl Drop for Cleanup<'_> {
        fn drop(&mut self) {
            for key in &self.keys {
                let _ = (self.delete.handler)(&dict_arg(&[
                    ("account", vm_string(&self.account)),
                    ("key", vm_string(key)),
                ]));
            }
        }
    }
    let _cleanup = Cleanup {
        delete,
        account: account.clone(),
        keys: vec![key1, key2],
    };

    (set.handler)(&dict_arg(&[
        ("account", vm_string(&account)),
        ("key", vm_string(key1)),
        ("value", vm_string("value-1")),
    ]))
    .unwrap();
    (set.handler)(&dict_arg(&[
        ("account", vm_string(&account)),
        ("key", vm_string(key2)),
        ("value", vm_string("value-2")),
    ]))
    .unwrap();

    let g1 = (get.handler)(&dict_arg(&[
        ("account", vm_string(&account)),
        ("key", vm_string(key1)),
    ]))
    .unwrap();
    assert_eq!(as_string(dict_get(&g1, "value")), "value-1");
    let backend = as_string(dict_get(&g1, "backend"));
    assert!(
        backend == "keychain" || backend == "wincred",
        "expected OS-native backend, got {backend}"
    );

    let listed = (list.handler)(&dict_arg(&[("account", vm_string(&account))])).unwrap();
    let keys = match dict_get(&listed, "keys") {
        VmValue::List(items) => items
            .iter()
            .map(|v| as_string(v).to_string())
            .collect::<Vec<_>>(),
        other => panic!("expected list, got {other:?}"),
    };
    assert!(keys.contains(&key1.to_string()) && keys.contains(&key2.to_string()));

    let del = (delete.handler)(&dict_arg(&[
        ("account", vm_string(&account)),
        ("key", vm_string(key1)),
    ]))
    .unwrap();
    assert!(matches!(dict_get(&del, "deleted"), VmValue::Bool(true)));

    let g_gone = (get.handler)(&dict_arg(&[
        ("account", vm_string(&account)),
        ("key", vm_string(key1)),
    ]))
    .unwrap();
    assert!(matches!(dict_get(&g_gone, "value"), VmValue::Nil));
}

#[test]
fn os_native_overwrite_replaces_existing_value() {
    if skip_if_file_backend_forced() {
        eprintln!("skipping: HARN_SECRET_STORE_BACKEND=file is set");
        return;
    }
    let reg = registry();
    let account = unique_account("overwrite");
    let key = "only";
    let set = reg.find("hostlib_secret_store_set").unwrap();
    let get = reg.find("hostlib_secret_store_get").unwrap();
    let delete = reg.find("hostlib_secret_store_delete").unwrap();

    let _cleanup = scopeguard_delete(delete, account.clone(), key);

    (set.handler)(&dict_arg(&[
        ("account", vm_string(&account)),
        ("key", vm_string(key)),
        ("value", vm_string("first")),
    ]))
    .unwrap();
    (set.handler)(&dict_arg(&[
        ("account", vm_string(&account)),
        ("key", vm_string(key)),
        ("value", vm_string("second")),
    ]))
    .unwrap();

    let g = (get.handler)(&dict_arg(&[
        ("account", vm_string(&account)),
        ("key", vm_string(key)),
    ]))
    .unwrap();
    assert_eq!(as_string(dict_get(&g, "value")), "second");
}

/// Lightweight RAII helper: deletes the named key on drop.
fn scopeguard_delete<'a>(
    delete: &'a harn_hostlib::RegisteredBuiltin,
    account: String,
    key: &'static str,
) -> impl Drop + 'a {
    struct Guard<'a> {
        delete: &'a harn_hostlib::RegisteredBuiltin,
        account: String,
        key: &'static str,
    }
    impl Drop for Guard<'_> {
        fn drop(&mut self) {
            let _ = (self.delete.handler)(&dict_arg(&[
                ("account", vm_string(&self.account)),
                ("key", vm_string(self.key)),
            ]));
        }
    }
    Guard {
        delete,
        account,
        key,
    }
}
