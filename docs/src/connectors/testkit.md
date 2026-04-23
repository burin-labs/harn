# Connector testkit

`harn_vm::connectors::testkit` is the shared fixture surface for Harn core and
connector package tests. It keeps connector tests deterministic without live
provider credentials, external services, or wall-clock sleeps.

Connector crates should add `harn-vm` as a dev-dependency and import only the
testkit pieces they need:

```rust,ignore
use harn_vm::connectors::testkit::{
    ConnectorTestkit, HttpMockGuard, HttpMockResponse, MemorySecretProvider,
    github_ping_fixture, scoped_secret_id,
};
```

## Runtime context

`ConnectorTestkit::new(start)` builds a complete `ConnectorCtx` backed by:

- a memory event log
- an inbox index and metrics registry
- a versioned in-memory `SecretProvider`
- a rate limiter
- a mock clock that can be installed for the current test thread

```rust,ignore
let kit = ConnectorTestkit::new(start).await;
let _clock = kit.install_clock();
connector.init(kit.ctx()).await?;
```

Use `kit.clock.advance_std(...)` or `advance_until(...)` to drive deadlines,
retry backoff, cron ticks, and cancellation logic without sleeping.

## Secrets

`MemorySecretProvider` supports latest and exact versions. The
`scoped_secret_id(namespace, tenant, binding, name)` helper gives connector
tests a consistent tenant/binding naming convention:

```rust,ignore
let secret_id = scoped_secret_id("github", "tenant-a", "binding-a", "token");
let secrets = MemorySecretProvider::new("github").with_secret(secret_id, "token-v1");
```

Use this for webhook signing keys, outbound tokens, and token-refresh tests.

## HTTP

`HttpMockGuard` drives the same mock registry used by Harn `http_request` and
the `http_mock(...)` builtins. Script-level and Rust-level assertions therefore
observe the same calls:

```rust,ignore
let http = HttpMockGuard::new();
http.push(
    "GET",
    "https://api.example.com/*",
    vec![HttpMockResponse::new(200, r#"{"ok":true}"#)],
);

let calls = http.calls();
```

The guard clears HTTP mock state on creation and drop.

## Streams and webhooks

`mock_stream()` returns a handle and reader for deterministic stream tests.
Send JSON or bytes through the handle, then call `cancel()` to prove stream
shutdown without a network socket.

Webhook fixtures cover common first-party shapes:

- `github_ping_fixture(secret, received_at)`
- `slack_message_fixture(secret, timestamp, received_at)`
- `linear_issue_update_fixture(secret, received_at)`

They return a `WebhookFixture` containing the signed `RawInbound` and original
body bytes. Use `.with_binding(...)` and `.with_tenant(...)` to attach runtime
metadata before normalization.

## Temp packages

`TempPackageWorkspace` creates a disposable package root and writes common
manifest markers for package-manager and conformance tests:

```rust,ignore
let workspace = TempPackageWorkspace::new("connector-contract")?;
workspace.write_harn_package("demo-connector")?;
workspace.write_file("src/main.harn", "pipeline main(task) {}")?;
```

The directory is removed when the workspace value drops.
