# Secret store (hostlib)

The `secret_store` capability is a small, sync host primitive for storing
per-application credentials in the operating system's native secret store,
with a portable JSON file fallback for headless environments. It is
registered automatically by `harn_hostlib::install_default` and exposes
four builtins:

| Builtin                            | Returns                                    |
|------------------------------------|--------------------------------------------|
| `hostlib_secret_store_get`         | `{account, key, value, backend}`           |
| `hostlib_secret_store_set`         | `{account, key, backend}`                  |
| `hostlib_secret_store_delete`      | `{account, key, deleted, backend}`         |
| `hostlib_secret_store_list`        | `{account, keys, backend}`                 |

`value` is `nil` when the key is absent. `backend` is one of
`"keychain"`, `"wincred"`, or `"file"` so callers can surface backend
status in diagnostics without re-deriving it.

## Backend selection

The active backend is resolved on every call (selection is essentially
free) so an env-var override takes effect without a process restart.

| OS               | Default backend                                              |
|------------------|--------------------------------------------------------------|
| macOS / iOS      | Apple Keychain (`security-framework`, generic password item) |
| Windows          | Credential Manager (`CredRead`/`CredWrite`, generic type)    |
| Linux and other  | File backend at `$XDG_CONFIG_HOME/<account>/credentials.json`|

### Forcing the file backend

Set `HARN_SECRET_STORE_BACKEND=file` to use the file backend regardless of
OS. This is the right knob for sandboxed CI, eval harnesses, container
images that must not touch the user's keychain, and anything else that
needs deterministic, file-based storage.

### Account namespacing

The `account` argument scopes every key to the calling application
(`my-app`, `cloud-admin`, etc.) so two applications can use the same
key name without collision:

* **Keychain** — `account` maps to `kSecAttrService`; `key` maps to
  `kSecAttrAccount`. Matches the layout used by a legacy Swift
  `KeychainCredentialStore` in an IDE host so existing entries are
  reachable without migration.
* **Credential Manager** — `account/key` becomes the credential's target
  name (e.g. `my-app/OPENAI_API_KEY`).
* **File backend** — credentials land at
  `$XDG_CONFIG_HOME/<account>/credentials.json`
  (or `%APPDATA%\<account>\credentials.json` on Windows). The file is
  written with `0o600` on Unix; the containing directory with `0o700`.

The path layout for the file backend is byte-compatible with an IDE host's
existing `$XDG_CONFIG_HOME/<account>/credentials.json` layout, so existing
deployments migrate without any data movement.

## Scope and non-goals

The capability owns *where the bytes live* and nothing else. Audit logging,
env-vs-stored precedence, schema validation beyond builtin signatures, and
migration logic belong in the `.harn` orchestration layer that composes
this primitive. Concretely:

* No "convenience" helpers like `effectiveValue(envKey, fallback)` — the
  caller decides whether to read `env`, the secret store, or both.
* No rotation, versioning, or zeroizing buffers. Those live in
  `harn-vm::secrets` (the async `SecretProvider` chain used by the LLM
  caller), which solves a different problem.
* No hardware-backed key stores. Deferred to a follow-up if any consumer
  asks.

## Verifying a setup

```text
HARN_SECRET_STORE_BACKEND=file cargo test -p harn-hostlib --test harn_hostlib secret_store
```

`tests/harn_hostlib/secret_store.rs` (file backend) runs on every CI runner.
`tests/harn_hostlib/secret_store_os_native.rs` exercises the Keychain (macOS)
and Credential Manager (Windows) backends end-to-end; on Linux it compiles
down to nothing because there is no OS-native backend yet.

## Open follow-up

* **Linux libsecret / secret-service backend.** Implemented as a wrapper
  around the `keyring` crate's `linux-native` feature when a consumer
  needs it — `harn-vm::secrets::KeyringSecretProvider` already pulls the
  same crate, so the dependency is free. Headless Linux CI keeps using
  the file backend (the same fallback `HARN_SECRET_STORE_BACKEND=file`
  selects) so behavior is identical to today.
