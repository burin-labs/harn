//! `std/oauth/dynamic_registration` — RFC 7591 / RFC 8414 client metadata
//! publishing and dynamic client registration (OA-05, issue #1906).
//!
//! This module is the *worker-side* counterpart to the OAuth client. It
//! lets a Harn-hosted service:
//!
//!   1. Build & validate RFC 7591 *client metadata* (`build_client_metadata`)
//!      that is served at `/.well-known/oauth-client.json` (configurable).
//!   2. Build RFC 8414 *authorization-server metadata*
//!      (`build_authorization_server_metadata`) served at
//!      `/.well-known/oauth-authorization-server.json`.
//!   3. Run an in-process dynamic-client-registration store backed by a
//!      thread-local `BTreeMap`, with `register / get / list` operations.
//!      Each successful `register` issues a fresh `client_id` +
//!      `client_secret` (URL-safe base64, 256 bits of entropy each) and
//!      stores the registration timestamp.
//!
//! ## Security
//!
//! All validation happens once in Rust so the rules are uniform across
//! the script-facing `build_client_metadata` helper and the
//! `register_client` registration path. The RFC 7591 §2 requirements
//! enforced here are intentionally conservative:
//!
//!   * `redirect_uris` must be a non-empty list of absolute `https://`
//!     URIs (or `http://localhost`/`http://127.0.0.1` for local-dev
//!     redirect URIs, with no fragment).
//!   * `grant_types`, `response_types`, and `token_endpoint_auth_method`
//!     are restricted to the spec-blessed enums.
//!   * `client_name`, `client_uri`, `logo_uri`, `tos_uri`, `policy_uri`,
//!     and `software_id`/`software_version` are bounded-length strings.
//!
//! Servers are explicitly permitted by RFC 7591 §5.1 to reject otherwise
//! conforming registrations; we use that latitude to refuse fragments,
//! relative URIs, and non-`https` redirect targets that are not loopback.
//!
//! The freshly minted `client_secret` is *only* returned from the
//! `register` call. Subsequent `get` reads omit it so logs and audit
//! events never accidentally re-emit it. The Harn-side wrapper module
//! is responsible for emitting `oauth.dynreg.audit` event-log entries
//! with the redacted shape.
//!
//! ## Concurrency
//!
//! The store lives in a `thread_local!` `RefCell<BTreeMap<...>>` keyed
//! by handle id (mirroring `oauth_storage`'s memory backend). The Harn
//! VM is single-threaded per execution, so a thread-local map is the
//! natural fit. Two `register` calls cannot mint colliding `client_id`s
//! because they are independently drawn from 256 bits of cryptographic
//! randomness.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Mutex;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use rand::Rng as _;
use serde_json::Value as JsonValue;
use url::Url;

use crate::llm::vm_value_to_json;
use crate::schema::json_to_vm_value;
use crate::stdlib::clock;
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

const HANDLE_KEY_KIND: &str = "kind";
const HANDLE_KEY_ID: &str = "id";
const KIND_DYNREG_STORE: &str = "oauth_dynreg_store";

const MAX_REDIRECT_URIS: usize = 16;
const MAX_STRING_LEN: usize = 2048;
const MAX_CLIENT_NAME_LEN: usize = 256;

// RFC 7591 §2 — allowed grant_types we accept on registration.
const ALLOWED_GRANT_TYPES: &[&str] = &[
    "authorization_code",
    "refresh_token",
    "client_credentials",
    "urn:ietf:params:oauth:grant-type:device_code",
    "urn:ietf:params:oauth:grant-type:jwt-bearer",
];

const ALLOWED_RESPONSE_TYPES: &[&str] = &["code", "token", "id_token", "code id_token", "none"];

const ALLOWED_AUTH_METHODS: &[&str] = &[
    "client_secret_basic",
    "client_secret_post",
    "client_secret_jwt",
    "private_key_jwt",
    "none",
];

// A persisted client registration. Metadata is serialized to JSON so the
// store can live in a thread-local map without holding `!Send` `VmValue`
// payloads. The freshly minted `client_secret` is intentionally NOT
// retained here — only the original `register` call returns it, and any
// later credential check would happen at the transport layer (e.g. by
// verifying a presented secret against an HMAC stored elsewhere). This
// keeps the in-process catalogue limited to the public registration
// shape and makes it impossible for a stray `get` to surface the secret.
#[derive(Clone, Debug)]
struct StoredClient {
    metadata: JsonValue,
    client_id: String,
    client_id_issued_at: i64,
}

#[derive(Default)]
struct DynregStore {
    clients: BTreeMap<String, StoredClient>,
}

thread_local! {
    static DYNREG_STORES: RefCell<BTreeMap<String, DynregStore>> =
        const { RefCell::new(BTreeMap::new()) };
}

static STORE_ID_COUNTER: Mutex<u64> = Mutex::new(0);

pub(crate) fn register_oauth_dynreg_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

pub(crate) const MODULE_BUILTINS: &[&crate::stdlib::macros::VmBuiltinDef] = &[
    &OAUTH_DYNREG_STORE_HANDLE_IMPL_DEF,
    &OAUTH_DYNREG_VALIDATE_METADATA_IMPL_DEF,
    &OAUTH_DYNREG_BUILD_CLIENT_METADATA_IMPL_DEF,
    &OAUTH_DYNREG_BUILD_AUTHORIZATION_SERVER_METADATA_IMPL_DEF,
    &OAUTH_DYNREG_REGISTER_IMPL_DEF,
    &OAUTH_DYNREG_GET_IMPL_DEF,
    &OAUTH_DYNREG_LIST_IMPL_DEF,
];

#[crate::stdlib::macros::harn_builtin(
    sig = "__oauth_dynreg_store_handle() -> dict",
    category = "oauth_dynreg"
)]
fn oauth_dynreg_store_handle_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    if !args.is_empty() {
        return Err(VmError::Runtime(
            "__oauth_dynreg_store_handle: expected 0 arguments".to_string(),
        ));
    }
    let id = next_store_id();
    DYNREG_STORES.with(|stores| {
        stores
            .borrow_mut()
            .insert(id.clone(), DynregStore::default());
    });
    Ok(store_handle(&id))
}

#[crate::stdlib::macros::harn_builtin(
    sig = "__oauth_dynreg_validate_metadata(metadata: dict) -> dict",
    category = "oauth_dynreg"
)]
fn oauth_dynreg_validate_metadata_impl(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let metadata = require_dict_arg(args, 0, "__oauth_dynreg_validate_metadata", "metadata")?;
    Ok(validate_metadata_value(&metadata))
}

#[crate::stdlib::macros::harn_builtin(
    sig = "__oauth_dynreg_build_client_metadata(metadata: dict) -> dict",
    category = "oauth_dynreg"
)]
fn oauth_dynreg_build_client_metadata_impl(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let metadata = require_dict_arg(args, 0, "__oauth_dynreg_build_client_metadata", "metadata")?;
    build_client_metadata_value(&metadata)
}

#[crate::stdlib::macros::harn_builtin(
    sig = "__oauth_dynreg_build_authorization_server_metadata(provider: dict, overrides?: dict) -> dict",
    category = "oauth_dynreg"
)]
fn oauth_dynreg_build_authorization_server_metadata_impl(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let provider = require_dict_arg(
        args,
        0,
        "__oauth_dynreg_build_authorization_server_metadata",
        "provider",
    )?;
    let overrides = optional_dict_arg(
        args,
        1,
        "__oauth_dynreg_build_authorization_server_metadata",
        "overrides",
    )?;
    build_authorization_server_metadata_value(&provider, overrides.as_ref())
}

#[crate::stdlib::macros::harn_builtin(
    sig = "__oauth_dynreg_register(handle: dict, metadata: dict) -> dict",
    category = "oauth_dynreg"
)]
fn oauth_dynreg_register_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let handle = require_handle(args, 0, "__oauth_dynreg_register")?;
    let metadata = require_dict_arg(args, 1, "__oauth_dynreg_register", "metadata")?;
    register_client_value(&handle, &metadata)
}

#[crate::stdlib::macros::harn_builtin(
    sig = "__oauth_dynreg_get(handle: dict, client_id: string) -> dict",
    category = "oauth_dynreg"
)]
fn oauth_dynreg_get_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let handle = require_handle(args, 0, "__oauth_dynreg_get")?;
    let client_id = required_string_arg(args, 1, "__oauth_dynreg_get", "client_id")?;
    Ok(get_client_value(&handle, &client_id))
}

#[crate::stdlib::macros::harn_builtin(
    sig = "__oauth_dynreg_list(handle: dict) -> list",
    category = "oauth_dynreg"
)]
fn oauth_dynreg_list_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let handle = require_handle(args, 0, "__oauth_dynreg_list")?;
    Ok(list_clients_value(&handle))
}

fn next_store_id() -> String {
    let mut guard = STORE_ID_COUNTER.lock().expect("dynreg counter poisoned");
    *guard = guard.wrapping_add(1);
    format!("dynreg-{}", *guard)
}

fn store_handle(id: &str) -> VmValue {
    let mut fields = BTreeMap::new();
    fields.insert(HANDLE_KEY_KIND.to_string(), string_value(KIND_DYNREG_STORE));
    fields.insert(HANDLE_KEY_ID.to_string(), string_value(id));
    VmValue::Dict(Rc::new(fields))
}

fn string_value(value: &str) -> VmValue {
    VmValue::String(Rc::from(value))
}

fn handle_id(handle: &BTreeMap<String, VmValue>) -> Result<String, VmError> {
    let kind = handle.get(HANDLE_KEY_KIND).and_then(|value| match value {
        VmValue::String(s) => Some(s.to_string()),
        _ => None,
    });
    match kind.as_deref() {
        Some(KIND_DYNREG_STORE) => {}
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "oauth dynamic registration: expected store handle kind `{KIND_DYNREG_STORE}`, got `{other}`"
            )));
        }
        None => {
            return Err(VmError::Runtime(
                "oauth dynamic registration: handle is missing `kind`".to_string(),
            ));
        }
    }
    handle
        .get(HANDLE_KEY_ID)
        .and_then(|value| match value {
            VmValue::String(s) => Some(s.to_string()),
            _ => None,
        })
        .ok_or_else(|| {
            VmError::Runtime("oauth dynamic registration: handle is missing `id`".to_string())
        })
}

// ----- Validation -----------------------------------------------------------

fn validate_metadata_value(metadata: &BTreeMap<String, VmValue>) -> VmValue {
    let mut errors: Vec<String> = Vec::new();
    validate_metadata(metadata, &mut errors);
    let mut out = BTreeMap::new();
    out.insert("ok".to_string(), VmValue::Bool(errors.is_empty()));
    out.insert(
        "errors".to_string(),
        VmValue::List(Rc::new(
            errors.iter().map(|e| string_value(e)).collect::<Vec<_>>(),
        )),
    );
    VmValue::Dict(Rc::new(out))
}

/// Run RFC 7591 §2 validation, pushing human-readable errors. Each error
/// is prefixed `HARN-OAU-005: ` so callers can pattern-match consistently.
fn validate_metadata(metadata: &BTreeMap<String, VmValue>, errors: &mut Vec<String>) {
    // redirect_uris: required, non-empty list, https or loopback http, no
    // fragments. RFC 7591 §2 + §5 — server may reject otherwise.
    let redirect_uris = metadata.get("redirect_uris");
    match redirect_uris {
        None | Some(VmValue::Nil) => {
            errors.push(err("redirect_uris is required"));
        }
        Some(VmValue::List(items)) => {
            if items.is_empty() {
                errors.push(err("redirect_uris must contain at least one entry"));
            } else if items.len() > MAX_REDIRECT_URIS {
                errors.push(err(&format!(
                    "redirect_uris must contain at most {MAX_REDIRECT_URIS} entries"
                )));
            }
            for (i, value) in items.iter().enumerate() {
                match value {
                    VmValue::String(s) => {
                        if let Err(reason) = validate_redirect_uri(s) {
                            errors.push(err(&format!("redirect_uris[{i}] {reason}")));
                        }
                    }
                    other => errors.push(err(&format!(
                        "redirect_uris[{i}] must be a string, got {}",
                        other.type_name()
                    ))),
                }
            }
        }
        Some(other) => errors.push(err(&format!(
            "redirect_uris must be a list of strings, got {}",
            other.type_name()
        ))),
    }

    // Optional list-of-string enums.
    validate_enum_list(metadata, "grant_types", ALLOWED_GRANT_TYPES, true, errors);
    validate_enum_list(
        metadata,
        "response_types",
        ALLOWED_RESPONSE_TYPES,
        true,
        errors,
    );

    // token_endpoint_auth_method: single optional string.
    if let Some(value) = metadata.get("token_endpoint_auth_method") {
        match value {
            VmValue::Nil => {}
            VmValue::String(s) => {
                if !ALLOWED_AUTH_METHODS.contains(&s.as_ref()) {
                    errors.push(err(&format!(
                        "token_endpoint_auth_method `{s}` is not one of: {}",
                        ALLOWED_AUTH_METHODS.join(", ")
                    )));
                }
            }
            other => errors.push(err(&format!(
                "token_endpoint_auth_method must be a string, got {}",
                other.type_name()
            ))),
        }
    }

    // scope: space-delimited string per RFC 7591 §2.
    if let Some(value) = metadata.get("scope") {
        match value {
            VmValue::Nil => {}
            VmValue::String(s) => {
                if s.len() > MAX_STRING_LEN {
                    errors.push(err(&format!("scope exceeds {MAX_STRING_LEN}-byte limit")));
                }
            }
            other => errors.push(err(&format!(
                "scope must be a string, got {}",
                other.type_name()
            ))),
        }
    }

    // Bounded display strings.
    validate_optional_bounded_string(metadata, "client_name", MAX_CLIENT_NAME_LEN, errors);
    validate_optional_url(metadata, "client_uri", errors);
    validate_optional_url(metadata, "logo_uri", errors);
    validate_optional_url(metadata, "tos_uri", errors);
    validate_optional_url(metadata, "policy_uri", errors);
    validate_optional_url(metadata, "jwks_uri", errors);
    validate_optional_bounded_string(metadata, "software_id", MAX_STRING_LEN, errors);
    validate_optional_bounded_string(metadata, "software_version", MAX_STRING_LEN, errors);

    // contacts: optional list of strings.
    if let Some(value) = metadata.get("contacts") {
        match value {
            VmValue::Nil => {}
            VmValue::List(items) => {
                for (i, item) in items.iter().enumerate() {
                    match item {
                        VmValue::String(s) => {
                            if s.len() > MAX_STRING_LEN {
                                errors.push(err(&format!(
                                    "contacts[{i}] exceeds {MAX_STRING_LEN}-byte limit"
                                )));
                            }
                        }
                        other => errors.push(err(&format!(
                            "contacts[{i}] must be a string, got {}",
                            other.type_name()
                        ))),
                    }
                }
            }
            other => errors.push(err(&format!(
                "contacts must be a list of strings, got {}",
                other.type_name()
            ))),
        }
    }
}

fn err(msg: &str) -> String {
    format!("HARN-OAU-005: {msg}")
}

fn validate_redirect_uri(value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err("must be a non-empty absolute URI".to_string());
    }
    if value.len() > MAX_STRING_LEN {
        return Err(format!("exceeds {MAX_STRING_LEN}-byte limit"));
    }
    let url = Url::parse(value).map_err(|e| format!("is not a valid absolute URI: {e}"))?;
    if url.fragment().is_some() {
        return Err("must not contain a URI fragment per RFC 7591 §2".to_string());
    }
    match url.scheme() {
        "https" => Ok(()),
        "http" => {
            // Allow only loopback for development. RFC 8252 §7.3.
            let host = url
                .host_str()
                .ok_or_else(|| "must specify a host".to_string())?;
            if host == "localhost" || host == "127.0.0.1" || host == "[::1]" {
                Ok(())
            } else {
                Err(format!(
                    "scheme `http` is only allowed for loopback hosts, got `{host}`"
                ))
            }
        }
        scheme => Err(format!(
            "scheme `{scheme}` is not allowed; use `https` or loopback `http`"
        )),
    }
}

fn validate_enum_list(
    metadata: &BTreeMap<String, VmValue>,
    field: &str,
    allowed: &[&str],
    optional: bool,
    errors: &mut Vec<String>,
) {
    match metadata.get(field) {
        None | Some(VmValue::Nil) => {
            if !optional {
                errors.push(err(&format!("{field} is required")));
            }
        }
        Some(VmValue::List(items)) => {
            for (i, item) in items.iter().enumerate() {
                match item {
                    VmValue::String(s) => {
                        if !allowed.contains(&s.as_ref()) {
                            errors.push(err(&format!(
                                "{field}[{i}] `{s}` is not one of: {}",
                                allowed.join(", ")
                            )));
                        }
                    }
                    other => errors.push(err(&format!(
                        "{field}[{i}] must be a string, got {}",
                        other.type_name()
                    ))),
                }
            }
        }
        Some(other) => errors.push(err(&format!(
            "{field} must be a list of strings, got {}",
            other.type_name()
        ))),
    }
}

fn validate_optional_bounded_string(
    metadata: &BTreeMap<String, VmValue>,
    field: &str,
    max_len: usize,
    errors: &mut Vec<String>,
) {
    if let Some(value) = metadata.get(field) {
        match value {
            VmValue::Nil => {}
            VmValue::String(s) => {
                if s.len() > max_len {
                    errors.push(err(&format!("{field} exceeds {max_len}-byte limit")));
                }
            }
            other => errors.push(err(&format!(
                "{field} must be a string, got {}",
                other.type_name()
            ))),
        }
    }
}

fn validate_optional_url(
    metadata: &BTreeMap<String, VmValue>,
    field: &str,
    errors: &mut Vec<String>,
) {
    if let Some(value) = metadata.get(field) {
        match value {
            VmValue::Nil => {}
            VmValue::String(s) => {
                if s.len() > MAX_STRING_LEN {
                    errors.push(err(&format!("{field} exceeds {MAX_STRING_LEN}-byte limit")));
                } else if let Err(e) = Url::parse(s) {
                    errors.push(err(&format!("{field} is not a valid absolute URI: {e}")));
                }
            }
            other => errors.push(err(&format!(
                "{field} must be a string, got {}",
                other.type_name()
            ))),
        }
    }
}

// ----- Metadata builders ----------------------------------------------------

fn build_client_metadata_value(metadata: &BTreeMap<String, VmValue>) -> Result<VmValue, VmError> {
    let mut errors: Vec<String> = Vec::new();
    validate_metadata(metadata, &mut errors);
    if !errors.is_empty() {
        return Err(VmError::Runtime(format!(
            "oauth dynamic registration: invalid client metadata: {}",
            errors.join("; ")
        )));
    }

    // Apply RFC 7591 §2 defaults when fields are omitted: response_types
    // defaults to `["code"]`; grant_types defaults to `["authorization_code"]`;
    // token_endpoint_auth_method defaults to `client_secret_basic`.
    let mut out: BTreeMap<String, VmValue> = metadata.clone();
    out.entry("response_types".to_string())
        .or_insert_with(|| VmValue::List(Rc::new(vec![string_value("code")])));
    out.entry("grant_types".to_string())
        .or_insert_with(|| VmValue::List(Rc::new(vec![string_value("authorization_code")])));
    out.entry("token_endpoint_auth_method".to_string())
        .or_insert_with(|| string_value("client_secret_basic"));
    Ok(VmValue::Dict(Rc::new(out)))
}

fn build_authorization_server_metadata_value(
    provider: &BTreeMap<String, VmValue>,
    overrides: Option<&BTreeMap<String, VmValue>>,
) -> Result<VmValue, VmError> {
    let auth_url = require_string_field(provider, "auth_url", "provider")?;
    let token_url = require_string_field(provider, "token_url", "provider")?;
    let issuer = derive_issuer(&auth_url)?;

    let mut out: BTreeMap<String, VmValue> = BTreeMap::new();
    out.insert("issuer".to_string(), string_value(&issuer));
    out.insert(
        "authorization_endpoint".to_string(),
        string_value(&auth_url),
    );
    out.insert("token_endpoint".to_string(), string_value(&token_url));
    if let Some(VmValue::String(s)) = provider.get("device_code_url") {
        out.insert(
            "device_authorization_endpoint".to_string(),
            string_value(s.as_ref()),
        );
    }
    if let Some(VmValue::String(s)) = provider.get("revoke_url") {
        out.insert("revocation_endpoint".to_string(), string_value(s.as_ref()));
    }
    if let Some(VmValue::String(s)) = provider.get("userinfo_url") {
        out.insert("userinfo_endpoint".to_string(), string_value(s.as_ref()));
    }
    out.insert(
        "response_types_supported".to_string(),
        VmValue::List(Rc::new(vec![string_value("code")])),
    );
    out.insert(
        "grant_types_supported".to_string(),
        VmValue::List(Rc::new(vec![
            string_value("authorization_code"),
            string_value("refresh_token"),
            string_value("urn:ietf:params:oauth:grant-type:device_code"),
        ])),
    );
    out.insert(
        "token_endpoint_auth_methods_supported".to_string(),
        VmValue::List(Rc::new(vec![
            string_value("client_secret_basic"),
            string_value("client_secret_post"),
            string_value("none"),
        ])),
    );
    if let Some(VmValue::Bool(true)) | Some(VmValue::Bool(false)) = provider.get("pkce_required") {
        out.insert(
            "code_challenge_methods_supported".to_string(),
            VmValue::List(Rc::new(vec![string_value("S256")])),
        );
    } else {
        out.insert(
            "code_challenge_methods_supported".to_string(),
            VmValue::List(Rc::new(vec![string_value("S256")])),
        );
    }
    if let Some(VmValue::List(scopes)) = provider.get("default_scopes") {
        out.insert(
            "scopes_supported".to_string(),
            VmValue::List(scopes.clone()),
        );
    }
    // Apply caller overrides last; this lets harn-cloud add e.g.
    // `registration_endpoint` once they wire the well-known route.
    if let Some(overrides) = overrides {
        for (k, v) in overrides {
            out.insert(k.clone(), v.clone());
        }
    }
    Ok(VmValue::Dict(Rc::new(out)))
}

fn derive_issuer(auth_url: &str) -> Result<String, VmError> {
    let url = Url::parse(auth_url).map_err(|e| {
        VmError::Runtime(format!(
            "oauth dynamic registration: provider.auth_url is not a valid URI: {e}"
        ))
    })?;
    let scheme = url.scheme();
    let host = url.host_str().ok_or_else(|| {
        VmError::Runtime(
            "oauth dynamic registration: provider.auth_url is missing a host".to_string(),
        )
    })?;
    match url.port() {
        Some(port) => Ok(format!("{scheme}://{host}:{port}")),
        None => Ok(format!("{scheme}://{host}")),
    }
}

// ----- Registration ---------------------------------------------------------

fn register_client_value(
    handle: &BTreeMap<String, VmValue>,
    metadata: &BTreeMap<String, VmValue>,
) -> Result<VmValue, VmError> {
    let mut errors: Vec<String> = Vec::new();
    validate_metadata(metadata, &mut errors);
    if !errors.is_empty() {
        return Err(VmError::Runtime(format!(
            "oauth dynamic registration: registration rejected: {}",
            errors.join("; ")
        )));
    }
    let store_id = handle_id(handle)?;
    let client_id = random_token(32);
    let client_secret = random_token(32);
    let issued_at = now_unix_seconds();

    let mut canonical = metadata.clone();
    canonical
        .entry("response_types".to_string())
        .or_insert_with(|| VmValue::List(Rc::new(vec![string_value("code")])));
    canonical
        .entry("grant_types".to_string())
        .or_insert_with(|| VmValue::List(Rc::new(vec![string_value("authorization_code")])));
    canonical
        .entry("token_endpoint_auth_method".to_string())
        .or_insert_with(|| string_value("client_secret_basic"));

    let canonical_dict = VmValue::Dict(Rc::new(canonical.clone()));
    let canonical_json = vm_value_to_json(&canonical_dict);

    let stored = StoredClient {
        metadata: canonical_json,
        client_id: client_id.clone(),
        client_id_issued_at: issued_at,
    };
    DYNREG_STORES.with(|stores| -> Result<(), VmError> {
        let mut stores = stores.borrow_mut();
        let store = stores.get_mut(&store_id).ok_or_else(|| {
            VmError::Runtime(format!(
                "oauth dynamic registration: store handle `{store_id}` is no longer registered"
            ))
        })?;
        store.clients.insert(client_id.clone(), stored);
        Ok(())
    })?;

    // Build the response dict — full RFC 7591 §3.2.1 success body.
    let mut response: BTreeMap<String, VmValue> = canonical;
    response.insert("client_id".to_string(), string_value(&client_id));
    response.insert("client_secret".to_string(), string_value(&client_secret));
    response.insert("client_id_issued_at".to_string(), VmValue::Int(issued_at));
    // Per RFC 7591 §3.2.1, `client_secret_expires_at: 0` means non-expiring.
    response.insert("client_secret_expires_at".to_string(), VmValue::Int(0));
    Ok(VmValue::Dict(Rc::new(response)))
}

fn get_client_value(handle: &BTreeMap<String, VmValue>, client_id: &str) -> VmValue {
    let store_id = match handle_id(handle) {
        Ok(v) => v,
        Err(_) => return VmValue::Nil,
    };
    DYNREG_STORES.with(|stores| {
        let stores = stores.borrow();
        let Some(store) = stores.get(&store_id) else {
            return VmValue::Nil;
        };
        let Some(client) = store.clients.get(client_id) else {
            return VmValue::Nil;
        };
        // Never re-emit the client_secret — only the original `register`
        // call returns it. Subsequent reads expose only the public
        // metadata + id + issued_at so logs/audits cannot accidentally
        // leak credentials.
        let metadata_value = json_to_vm_value(&client.metadata);
        let mut response: BTreeMap<String, VmValue> = match metadata_value {
            VmValue::Dict(dict) => dict.as_ref().clone(),
            _ => BTreeMap::new(),
        };
        response.insert("client_id".to_string(), string_value(&client.client_id));
        response.insert(
            "client_id_issued_at".to_string(),
            VmValue::Int(client.client_id_issued_at),
        );
        response.insert("client_secret_expires_at".to_string(), VmValue::Int(0));
        VmValue::Dict(Rc::new(response))
    })
}

fn list_clients_value(handle: &BTreeMap<String, VmValue>) -> VmValue {
    let store_id = match handle_id(handle) {
        Ok(v) => v,
        Err(_) => return VmValue::List(Rc::new(Vec::new())),
    };
    DYNREG_STORES.with(|stores| {
        let stores = stores.borrow();
        let Some(store) = stores.get(&store_id) else {
            return VmValue::List(Rc::new(Vec::new()));
        };
        let ids: Vec<VmValue> = store
            .clients
            .keys()
            .map(|id| string_value(id.as_str()))
            .collect();
        VmValue::List(Rc::new(ids))
    })
}

// ----- Helpers --------------------------------------------------------------

fn random_token(byte_len: usize) -> String {
    let mut buf = vec![0u8; byte_len];
    rand::rng().fill_bytes(&mut buf);
    URL_SAFE_NO_PAD.encode(&buf)
}

fn now_unix_seconds() -> i64 {
    // Route through `stdlib::clock` so mock-clock-based conformance / unit
    // tests get a deterministic `client_id_issued_at` instead of the host
    // wall clock. Falls back to the real system time when no mock is
    // installed.
    clock::now_wall_ms() / 1000
}

fn require_handle(
    args: &[VmValue],
    index: usize,
    fn_name: &str,
) -> Result<BTreeMap<String, VmValue>, VmError> {
    match args.get(index) {
        Some(VmValue::Dict(dict)) => Ok(dict.as_ref().clone()),
        Some(other) => Err(VmError::Runtime(format!(
            "{fn_name}: handle must be a dict, got {}",
            other.type_name()
        ))),
        None => Err(VmError::Runtime(format!(
            "{fn_name}: missing handle argument"
        ))),
    }
}

fn require_dict_arg(
    args: &[VmValue],
    index: usize,
    fn_name: &str,
    arg_name: &str,
) -> Result<BTreeMap<String, VmValue>, VmError> {
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

fn optional_dict_arg(
    args: &[VmValue],
    index: usize,
    fn_name: &str,
    arg_name: &str,
) -> Result<Option<BTreeMap<String, VmValue>>, VmError> {
    match args.get(index) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::Dict(dict)) => Ok(Some(dict.as_ref().clone())),
        Some(other) => Err(VmError::Runtime(format!(
            "{fn_name}: `{arg_name}` must be a dict or nil, got {}",
            other.type_name()
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

fn require_string_field(
    metadata: &BTreeMap<String, VmValue>,
    field: &str,
    owner: &str,
) -> Result<String, VmError> {
    match metadata.get(field) {
        Some(VmValue::String(s)) if !s.is_empty() => Ok(s.to_string()),
        Some(VmValue::String(_)) => Err(VmError::Runtime(format!(
            "oauth dynamic registration: {owner}.{field} must not be empty"
        ))),
        _ => Err(VmError::Runtime(format!(
            "oauth dynamic registration: {owner}.{field} is required"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata_with_redirect(uri: &str) -> BTreeMap<String, VmValue> {
        let mut m = BTreeMap::new();
        m.insert(
            "redirect_uris".to_string(),
            VmValue::List(Rc::new(vec![string_value(uri)])),
        );
        m
    }

    #[test]
    fn valid_metadata_passes() {
        let m = metadata_with_redirect("https://app.example/callback");
        let mut errors = Vec::new();
        validate_metadata(&m, &mut errors);
        assert!(errors.is_empty(), "expected no errors, got {errors:?}");
    }

    #[test]
    fn loopback_http_redirect_passes() {
        let m = metadata_with_redirect("http://127.0.0.1:8765/callback");
        let mut errors = Vec::new();
        validate_metadata(&m, &mut errors);
        assert!(errors.is_empty(), "expected no errors, got {errors:?}");
    }

    #[test]
    fn non_loopback_http_redirect_fails() {
        let m = metadata_with_redirect("http://attacker.example/callback");
        let mut errors = Vec::new();
        validate_metadata(&m, &mut errors);
        assert_eq!(errors.len(), 1, "expected single rejection, got {errors:?}");
        assert!(errors[0].contains("HARN-OAU-005"));
        assert!(errors[0].contains("loopback"), "got {}", errors[0]);
    }

    #[test]
    fn fragment_in_redirect_fails() {
        let m = metadata_with_redirect("https://app.example/callback#frag");
        let mut errors = Vec::new();
        validate_metadata(&m, &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("fragment"), "got {}", errors[0]);
    }

    #[test]
    fn empty_redirect_uris_fails() {
        let mut m = BTreeMap::new();
        m.insert(
            "redirect_uris".to_string(),
            VmValue::List(Rc::new(Vec::new())),
        );
        let mut errors = Vec::new();
        validate_metadata(&m, &mut errors);
        assert!(errors[0].contains("at least one"));
    }

    #[test]
    fn missing_redirect_uris_fails() {
        let m: BTreeMap<String, VmValue> = BTreeMap::new();
        let mut errors = Vec::new();
        validate_metadata(&m, &mut errors);
        assert!(errors[0].contains("redirect_uris is required"));
    }

    #[test]
    fn unknown_grant_type_fails() {
        let mut m = metadata_with_redirect("https://app.example/cb");
        m.insert(
            "grant_types".to_string(),
            VmValue::List(Rc::new(vec![string_value("password")])),
        );
        let mut errors = Vec::new();
        validate_metadata(&m, &mut errors);
        assert!(errors.iter().any(|e| e.contains("password")));
    }

    #[test]
    fn build_applies_defaults() {
        let m = metadata_with_redirect("https://app.example/cb");
        let built = build_client_metadata_value(&m).expect("ok");
        let VmValue::Dict(dict) = built else {
            panic!("expected dict")
        };
        assert!(dict.contains_key("grant_types"));
        assert!(dict.contains_key("response_types"));
        assert!(dict.contains_key("token_endpoint_auth_method"));
    }

    #[test]
    fn build_authorization_server_metadata_minimal() {
        let mut provider = BTreeMap::new();
        provider.insert(
            "auth_url".to_string(),
            string_value("https://idp.example/authorize"),
        );
        provider.insert(
            "token_url".to_string(),
            string_value("https://idp.example/token"),
        );
        let built = build_authorization_server_metadata_value(&provider, None).expect("ok");
        let VmValue::Dict(dict) = built else {
            panic!("expected dict")
        };
        assert!(
            matches!(dict.get("issuer"), Some(VmValue::String(s)) if s.as_ref() == "https://idp.example")
        );
        assert!(matches!(
            dict.get("authorization_endpoint"),
            Some(VmValue::String(_))
        ));
        assert!(matches!(
            dict.get("token_endpoint"),
            Some(VmValue::String(_))
        ));
        assert!(matches!(
            dict.get("response_types_supported"),
            Some(VmValue::List(_))
        ));
    }

    #[test]
    fn register_returns_client_id_and_secret() {
        // Fresh store.
        let id = next_store_id();
        DYNREG_STORES.with(|stores| {
            stores
                .borrow_mut()
                .insert(id.clone(), DynregStore::default());
        });
        let mut handle = BTreeMap::new();
        handle.insert("kind".to_string(), string_value(KIND_DYNREG_STORE));
        handle.insert("id".to_string(), string_value(&id));
        let m = metadata_with_redirect("https://app.example/cb");
        let response = register_client_value(&handle, &m).expect("registration ok");
        let VmValue::Dict(dict) = response else {
            panic!("expected dict")
        };
        let VmValue::String(client_id) = dict.get("client_id").expect("client_id") else {
            panic!("client_id must be a string");
        };
        assert!(client_id.len() >= 32);
        assert!(matches!(dict.get("client_secret"), Some(VmValue::String(s)) if s.len() >= 32));
        // Get reads do NOT expose the secret.
        let fetched = get_client_value(&handle, client_id);
        let VmValue::Dict(get_dict) = fetched else {
            panic!("expected dict on get")
        };
        assert!(get_dict.get("client_id").is_some());
        assert!(
            get_dict.get("client_secret").is_none(),
            "get must not leak client_secret"
        );
    }

    #[test]
    fn register_rejects_invalid_metadata() {
        let id = next_store_id();
        DYNREG_STORES.with(|stores| {
            stores
                .borrow_mut()
                .insert(id.clone(), DynregStore::default());
        });
        let mut handle = BTreeMap::new();
        handle.insert("kind".to_string(), string_value(KIND_DYNREG_STORE));
        handle.insert("id".to_string(), string_value(&id));
        let m = metadata_with_redirect("http://attacker.example/cb");
        let err = register_client_value(&handle, &m).unwrap_err();
        let VmError::Runtime(text) = err else {
            panic!("expected runtime error")
        };
        assert!(text.contains("HARN-OAU-005"));
    }
}
