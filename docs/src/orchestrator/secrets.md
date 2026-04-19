# Orchestrator Secrets

Reactive Harn features need a single way to fetch secrets without
sprinkling provider-specific code across connectors, OAuth flows, and
future orchestrator runtime surfaces. The secret layer lives in
`harn_vm::secrets` and currently ships with two concrete providers:

- `EnvSecretProvider`
- `KeyringSecretProvider`

The default chain is:

```text
env -> keyring
```

Use `harn doctor --no-network` to inspect the active chain and to verify
that the keyring backend is reachable on the current machine.

## Secret model

Secrets are addressed by a structured `SecretId`:

```rust
use harn_vm::secrets::{SecretId, SecretVersion};

let id = SecretId::new(
    "harn.orchestrator.github",
    "installation-12345/private-key",
)
.with_version(SecretVersion::Latest);
```

Secret values are held in `SecretBytes`:

- bytes are zeroized on drop
- `Debug` is redacted
- `Display` is intentionally absent
- explicit duplication requires `reborrow()`
- callers expose bytes via `with_exposed(|bytes| ...)`

Successful `get()` calls also emit a structured audit event through the
existing VM event sink with the secret id, provider name, caller span,
mutation session id when present, and a timestamp. The event payload never
contains the secret bytes.

## Provider chain configuration

The provider order is controlled with `HARN_SECRET_PROVIDERS`:

```bash
export HARN_SECRET_PROVIDERS=env,keyring
```

The doctor output also reports a namespace used for backend grouping. By
default Harn derives it as `harn/<current-directory-name>`. Override it
with:

```bash
export HARN_SECRET_NAMESPACE="harn/my-workspace"
```

## Environment provider

`EnvSecretProvider` is first in the chain so CI, local shells, and
containers can override secrets without touching the OS credential store.

Environment variable names use:

```text
HARN_SECRET_<NAMESPACE>_<NAME>
```

For example:

```bash
export HARN_SECRET_HARN_ORCHESTRATOR_GITHUB_INSTALLATION_12345_PRIVATE_KEY="$(cat github-app.pem)"
```

Non-alphanumeric characters are normalized to underscores and multiple
separators collapse.

## Keyring provider

`KeyringSecretProvider` uses the [`keyring`](https://crates.io/crates/keyring)
crate so the same code path works against:

- macOS Keychain
- Linux native keyring / Secret Service backends supported by `keyring`
- Windows Credential Manager

This is the default local-first provider. The CLI already uses it for MCP
OAuth token storage, and `harn doctor` probes it directly.

## Recommended setups

Laptop development:

```bash
export HARN_SECRET_PROVIDERS=env,keyring
```

CI or containers:

```bash
export HARN_SECRET_PROVIDERS=env
```

Cloud deployments:

Today, use `env` for injected platform secrets. The `SecretProvider`
surface is intentionally ready for Vault / AWS / GCP implementations, but
those provider backends are not wired in yet.
