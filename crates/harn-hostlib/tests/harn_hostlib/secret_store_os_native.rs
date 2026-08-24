//! OS-native secret-store smoke tests.
//!
//! Exercises the live keyring ecosystem adapter on macOS and Windows. These
//! tests:
//!
//! * Are skipped when `HARN_SECRET_STORE_BACKEND=file` is set in the
//!   environment — CI matrices that explicitly opt into the file backend
//!   should not trigger keychain prompts.
//! * Skip with a typed capability reason when the native store is linked but
//!   inaccessible to the current desktop session. A missing adapter remains a
//!   hard failure so feature-wiring regressions cannot pass vacuously.
//! * Use a unique account name per run so concurrent CI invocations and
//!   leftover entries from prior failed runs cannot cause cross-contamination.
//! * Always clean up after themselves, even on assertion failure, and VERIFY
//!   the removal by reading the key back rather than assuming the delete took.
//!
//! Linux uses Secret Service too, but headless CI generally has no unlocked
//! session collection, so live coverage stays on desktop runners.

#![cfg(any(target_os = "macos", target_os = "windows"))]

use std::sync::atomic::{AtomicU64, Ordering};

use harn_hostlib::{
    secret_store::SecretStoreCapability, BuiltinRegistry, HostlibCapability, HostlibError,
};
use harn_vm::{secrets::NativeKeyringUnavailable, VmValue};

fn registry() -> BuiltinRegistry {
    let mut registry = BuiltinRegistry::new();
    SecretStoreCapability.register_builtins(&mut registry);
    registry
}

fn dict_arg(entries: &[(&str, VmValue)]) -> Vec<VmValue> {
    let mut map: harn_vm::value::DictMap = Default::default();
    for (k, v) in entries {
        map.insert(harn_vm::value::intern_key(k), v.clone());
    }
    vec![VmValue::dict(map)]
}

fn vm_string(s: &str) -> VmValue {
    VmValue::String(arcstr::ArcStr::from(s))
}

fn dict_get<'a>(value: &'a VmValue, key: &str) -> &'a VmValue {
    match value {
        VmValue::Dict(d) => d.get(key).expect("key present"),
        other => panic!("not a dict: {other:?}"),
    }
}

fn as_string(value: &VmValue) -> &str {
    match value {
        VmValue::String(s) => s.as_str(),
        other => panic!("not a string: {other:?}"),
    }
}

/// Process-unique account name so parallel test invocations cannot
/// collide. The pid plus an atomic counter is sufficient: PIDs differ
/// across concurrent processes, the counter differs across calls within
/// a process, and every test cleans up after itself via `scopeguard_delete`
/// (or the inline `Cleanup` RAII helper), both of which verify the removal.
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

fn native_operation_or_skip(result: Result<VmValue, HostlibError>) -> bool {
    match result {
        Ok(_) => true,
        Err(HostlibError::NativeSecretStoreUnavailable {
            reason:
                reason @ (NativeKeyringUnavailable::StorageInaccessible
                | NativeKeyringUnavailable::InteractionRequired),
            message,
            ..
        }) => {
            eprintln!("skipping: native secret store unavailable ({reason}): {message}");
            false
        }
        Err(error) => panic!("native secret-store operation failed: {error}"),
    }
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

    // Always clean up, even if an assertion below fires — and prove it worked.
    struct Cleanup<'a> {
        delete: &'a harn_hostlib::RegisteredBuiltin,
        get: &'a harn_hostlib::RegisteredBuiltin,
        account: String,
        keys: Vec<&'static str>,
    }
    impl Drop for Cleanup<'_> {
        fn drop(&mut self) {
            for key in &self.keys {
                verify_deleted(self.delete, self.get, &self.account, key);
            }
        }
    }
    let _cleanup = Cleanup {
        delete,
        get,
        account: account.clone(),
        keys: vec![key1, key2],
    };

    if !native_operation_or_skip((set.handler)(&dict_arg(&[
        ("account", vm_string(&account)),
        ("key", vm_string(key1)),
        ("value", vm_string("value-1")),
    ]))) {
        return;
    }
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
    assert_eq!(backend, "keyring");

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

    let _cleanup = scopeguard_delete(delete, get, account.clone(), key);

    if !native_operation_or_skip((set.handler)(&dict_arg(&[
        ("account", vm_string(&account)),
        ("key", vm_string(key)),
        ("value", vm_string("first")),
    ]))) {
        return;
    }
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

/// Delete `key` and prove it is gone, rather than assuming it.
///
/// Teardown used to discard the delete result, so an item that survived looked
/// exactly like one that was cleaned up. That is how the leftover Keychain
/// entries in #6709 accumulated unnoticed across runs.
///
/// `delete` returning an error is not itself a leak — the key may already be
/// gone — so the check is the read-back the tests themselves establish: a key
/// that is absent reads as `value: nil`. Anything else is still in the user's
/// Keychain.
///
/// A leak discovered while the test is already unwinding is reported and not
/// re-panicked. Panicking in `Drop` during an unwind aborts the process, which
/// would replace the real assertion failure with a far less useful one.
fn verify_deleted(
    delete: &harn_hostlib::RegisteredBuiltin,
    get: &harn_hostlib::RegisteredBuiltin,
    account: &str,
    key: &str,
) {
    let delete_result = (delete.handler)(&dict_arg(&[
        ("account", vm_string(account)),
        ("key", vm_string(key)),
    ]));
    let read_back = (get.handler)(&dict_arg(&[
        ("account", vm_string(account)),
        ("key", vm_string(key)),
    ]));
    let read_back = match read_back {
        Ok(value) => value,
        // The store is unreachable — which is also how these tests skip. An
        // item we cannot read is not an item we can call leaked, and failing
        // here would turn every legitimate skip red. Say so and stop.
        Err(error) => {
            eprintln!(
                "keychain teardown for {account}/{key} could not verify removal \
                 (delete: {delete_result:?}, read-back: {error})"
            );
            return;
        }
    };
    if matches!(dict_get(&read_back, "value"), VmValue::Nil) {
        return;
    }
    let message = format!(
        "keychain teardown left {account}/{key} behind (delete: {delete_result:?}). \
         Remove it manually; later runs of this suite will otherwise read a stale value."
    );
    if std::thread::panicking() {
        eprintln!("{message}");
    } else {
        panic!("{message}");
    }
}

/// Lightweight RAII helper: deletes the named key on drop and verifies it went.
fn scopeguard_delete<'a>(
    delete: &'a harn_hostlib::RegisteredBuiltin,
    get: &'a harn_hostlib::RegisteredBuiltin,
    account: String,
    key: &'static str,
) -> impl Drop + 'a {
    struct Guard<'a> {
        delete: &'a harn_hostlib::RegisteredBuiltin,
        get: &'a harn_hostlib::RegisteredBuiltin,
        account: String,
        key: &'static str,
    }
    impl Drop for Guard<'_> {
        fn drop(&mut self) {
            verify_deleted(self.delete, self.get, &self.account, self.key);
        }
    }
    Guard {
        delete,
        get,
        account,
        key,
    }
}
