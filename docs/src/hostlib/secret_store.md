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

`value` is `nil` when the key is absent. `backend` is `"keyring"` or
`"file"` so callers can surface backend status without re-deriving it.

## Backend selection

The active backend is resolved on every call (selection is essentially
free) so an env-var override takes effect without a process restart.

| OS               | Default backend                                      |
|------------------|------------------------------------------------------|
| macOS            | Apple Keychain through `apple-native-keyring-store`  |
| iOS              | Protected Data through `apple-native-keyring-store`  |
| Windows          | Credential Manager through its keyring store crate   |
| Linux / Unix     | Secret Service through its keyring store crate       |
| Other targets    | File backend                                         |

### Forcing the file backend

Set `HARN_SECRET_STORE_BACKEND=file` to use the file backend regardless of
OS. This is the right knob for sandboxed CI, eval harnesses, container
images that must not touch the user's keychain, and anything else that
needs deterministic, file-based storage.

### Account namespacing

The `account` argument scopes every key to the calling application
(`my-app`, `cloud-admin`, etc.) so two applications can use the same
key name without collision:

* **Native keyring** — `account` maps to the ecosystem's `service` field and
  `key` maps to its `user` field. On Apple this retains the standard
  Keychain service/account mapping; the maintained platform adapters own the
  corresponding Credential Manager and Secret Service representations.
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
`tests/harn_hostlib/secret_store_os_native.rs` exercises desktop native stores
end-to-end on macOS and Windows. Linux builds and uses Secret Service by
default; headless jobs should force the deterministic file backend because
they generally have no unlocked desktop session collection.
