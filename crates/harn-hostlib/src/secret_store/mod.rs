//! Per-OS secret-store host primitive.
//!
//! Stores credentials in a per-application namespace (`account`) keyed by
//! a free-form `key`. The active backend is picked at call time:
//!
//! | OS               | Default backend                                              |
//! |------------------|--------------------------------------------------------------|
//! | macOS / iOS      | Apple Keychain (`security-framework`, generic password item) |
//! | Windows          | Credential Manager (`CredRead`/`CredWrite`, generic type)    |
//! | Linux / other    | File backend at `$XDG_CONFIG_HOME/<account>/credentials.json`|
//!
//! Setting `HARN_SECRET_STORE_BACKEND=file` forces the file backend on every
//! OS — useful for sandboxed CI, eval harnesses, and anything that must not
//! touch the user's keychain. Tests also set `HARN_SECRET_STORE_FILE_ROOT`
//! to redirect the file backend's root config directory.
//!
//! The four registered builtins are intentionally minimal — they own
//! "where the bytes live" and nothing else. Audit logging, schema
//! validation beyond builtin signatures, env-vs-stored precedence, and
//! migration logic belong in the `.harn` orchestration layer.

use std::io;
use std::sync::Arc;

use harn_vm::VmValue;

use crate::error::HostlibError;
use crate::registry::{BuiltinRegistry, HostlibCapability};
use crate::tools::args::{build_dict, dict_arg, require_string, str_value};

mod file;
#[cfg(any(target_os = "macos", target_os = "ios"))]
mod keychain;
#[cfg(target_os = "windows")]
mod wincred;

const GET_BUILTIN: &str = "hostlib_secret_store_get";
const SET_BUILTIN: &str = "hostlib_secret_store_set";
const DELETE_BUILTIN: &str = "hostlib_secret_store_delete";
const LIST_BUILTIN: &str = "hostlib_secret_store_list";

/// Backend-selection override. When set to `"file"` the file backend is
/// used unconditionally. Other values fall through to the OS default.
const BACKEND_ENV: &str = "HARN_SECRET_STORE_BACKEND";

/// Minimal backend contract every implementation honors.
trait Backend {
    fn name(&self) -> &'static str;
    fn get(&self, account: &str, key: &str) -> io::Result<Option<String>>;
    fn set(&self, account: &str, key: &str, value: &str) -> io::Result<()>;
    fn delete(&self, account: &str, key: &str) -> io::Result<bool>;
    fn list(&self, account: &str) -> io::Result<Vec<String>>;
}

/// Secret-store capability. Stateless: backend selection is resolved on
/// every call so environment-variable overrides (`HARN_SECRET_STORE_BACKEND`)
/// take effect without requiring process restart.
#[derive(Default)]
pub struct SecretStoreCapability;

impl HostlibCapability for SecretStoreCapability {
    fn module_name(&self) -> &'static str {
        "secret_store"
    }

    fn register_builtins(&self, registry: &mut BuiltinRegistry) {
        registry.register_fn("secret_store", GET_BUILTIN, "get", handle_get);
        registry.register_fn("secret_store", SET_BUILTIN, "set", handle_set);
        registry.register_fn("secret_store", DELETE_BUILTIN, "delete", handle_delete);
        registry.register_fn("secret_store", LIST_BUILTIN, "list", handle_list);
    }
}

fn handle_get(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let dict = dict_arg(GET_BUILTIN, args)?;
    let account = require_nonempty_string(GET_BUILTIN, &dict, "account")?;
    let key = require_nonempty_string(GET_BUILTIN, &dict, "key")?;

    let backend = select_backend();
    let value = backend
        .get(&account, &key)
        .map_err(|err| backend_err(GET_BUILTIN, err))?;

    Ok(build_dict([
        ("account".to_string(), str_value(&account)),
        ("key".to_string(), str_value(&key)),
        (
            "value".to_string(),
            value.as_deref().map(str_value).unwrap_or(VmValue::Nil),
        ),
        ("backend".to_string(), str_value(backend.name())),
    ]))
}

fn handle_set(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let dict = dict_arg(SET_BUILTIN, args)?;
    let account = require_nonempty_string(SET_BUILTIN, &dict, "account")?;
    let key = require_nonempty_string(SET_BUILTIN, &dict, "key")?;
    let value = require_string(SET_BUILTIN, &dict, "value")?;

    let backend = select_backend();
    backend
        .set(&account, &key, &value)
        .map_err(|err| backend_err(SET_BUILTIN, err))?;

    Ok(build_dict([
        ("account".to_string(), str_value(&account)),
        ("key".to_string(), str_value(&key)),
        ("backend".to_string(), str_value(backend.name())),
    ]))
}

fn handle_delete(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let dict = dict_arg(DELETE_BUILTIN, args)?;
    let account = require_nonempty_string(DELETE_BUILTIN, &dict, "account")?;
    let key = require_nonempty_string(DELETE_BUILTIN, &dict, "key")?;

    let backend = select_backend();
    let deleted = backend
        .delete(&account, &key)
        .map_err(|err| backend_err(DELETE_BUILTIN, err))?;

    Ok(build_dict([
        ("account".to_string(), str_value(&account)),
        ("key".to_string(), str_value(&key)),
        ("deleted".to_string(), VmValue::Bool(deleted)),
        ("backend".to_string(), str_value(backend.name())),
    ]))
}

fn handle_list(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let dict = dict_arg(LIST_BUILTIN, args)?;
    let account = require_nonempty_string(LIST_BUILTIN, &dict, "account")?;

    let backend = select_backend();
    let keys = backend
        .list(&account)
        .map_err(|err| backend_err(LIST_BUILTIN, err))?;

    let items: Vec<VmValue> = keys.into_iter().map(|k| str_value(&k)).collect();
    Ok(build_dict([
        ("account".to_string(), str_value(&account)),
        ("keys".to_string(), VmValue::List(Arc::new(items))),
        ("backend".to_string(), str_value(backend.name())),
    ]))
}

fn require_nonempty_string(
    builtin: &'static str,
    dict: &harn_vm::value::DictMap,
    key: &'static str,
) -> Result<String, HostlibError> {
    let value = require_string(builtin, dict, key)?;
    if value.is_empty() {
        Err(HostlibError::InvalidParameter {
            builtin,
            param: key,
            message: "must be a non-empty string".to_string(),
        })
    } else {
        Ok(value)
    }
}

fn backend_err(builtin: &'static str, err: io::Error) -> HostlibError {
    HostlibError::Backend {
        builtin,
        message: err.to_string(),
    }
}

fn select_backend() -> Box<dyn Backend> {
    if matches!(std::env::var(BACKEND_ENV).as_deref(), Ok("file")) {
        return Box::new(file::FileStore::new());
    }

    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        Box::new(keychain::KeychainStore::new())
    }
    #[cfg(target_os = "windows")]
    {
        Box::new(wincred::WinCredStore::new())
    }
    #[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "windows")))]
    {
        Box::new(file::FileStore::new())
    }
}
