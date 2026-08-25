#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::SecretProvider;
    use crate::VmValue;
    use async_trait::async_trait;

    #[derive(Default)]
    struct RecordingWorkspaceBridge {
        calls: Mutex<Vec<(String, String, crate::value::DictMap)>>,
    }

    impl crate::stdlib::host::HostCallBridge for RecordingWorkspaceBridge {
        fn dispatch<'a>(
            &'a self,
            capability: &'a str,
            operation: &'a str,
            params: &'a crate::value::DictMap,
        ) -> crate::stdlib::host::HostCallDispatchFuture<'a> {
            self.calls.lock().expect("calls poisoned").push((
                capability.to_string(),
                operation.to_string(),
                params.clone(),
            ));
            crate::stdlib::host::host_call_ready(Ok(Some(
                crate::stdlib::json_to_vm_value(
                    &serde_json::json!({"matches": ["src/runtime.rs"]}),
                ),
            )))
        }
    }

    struct HostBridgeGuard;

    impl Drop for HostBridgeGuard {
        fn drop(&mut self) {
            crate::stdlib::host::clear_host_call_bridge();
        }
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct SecretCall {
        operation: &'static str,
        id: crate::secrets::SecretId,
        scope: crate::secrets::SecretScope,
        request_id: Option<String>,
        actor_subject: Option<String>,
        actor_kind: Option<String>,
        duration_ms: Option<u64>,
        grace_ms: Option<u64>,
        ttl_ms: Option<u64>,
    }

    #[derive(Clone, Default)]
    struct RecordingSecretProvider {
        inner: Arc<RecordingSecretProviderInner>,
    }

    #[derive(Default)]
    struct RecordingSecretProviderInner {
        versions: Mutex<BTreeMap<crate::secrets::SecretId, Vec<Vec<u8>>>>,
        calls: Mutex<Vec<SecretCall>>,
    }

    impl RecordingSecretProvider {
        fn calls(&self) -> Vec<SecretCall> {
            self.inner
                .calls
                .lock()
                .expect("calls lock poisoned")
                .clone()
        }

        fn record(
            &self,
            operation: &'static str,
            id: &crate::secrets::SecretId,
            scope: &crate::secrets::SecretScope,
            audit: &crate::secrets::SecretAuditContext,
            duration_ms: Option<u64>,
            grace_ms: Option<u64>,
            ttl_ms: Option<u64>,
        ) {
            self.inner
                .calls
                .lock()
                .expect("calls lock poisoned")
                .push(SecretCall {
                    operation,
                    id: id.clone(),
                    scope: scope.clone(),
                    request_id: audit.request_id.clone(),
                    actor_subject: audit.actor_subject.clone(),
                    actor_kind: audit.actor_kind.clone(),
                    duration_ms,
                    grace_ms,
                    ttl_ms,
                });
        }

        fn read_latest(
            &self,
            id: &crate::secrets::SecretId,
        ) -> Result<(u64, Vec<u8>), crate::secrets::SecretError> {
            let versions = self.inner.versions.lock().expect("versions lock poisoned");
            let values = versions
                .get(id)
                .filter(|values| !values.is_empty())
                .ok_or_else(|| crate::secrets::SecretError::NotFound {
                    provider: self.namespace().to_string(),
                    id: id.clone(),
                })?;
            Ok((
                values.len() as u64,
                values.last().expect("non-empty").clone(),
            ))
        }

        fn write_version(
            &self,
            id: &crate::secrets::SecretId,
            value: &crate::secrets::SecretBytes,
        ) -> u64 {
            let mut versions = self.inner.versions.lock().expect("versions lock poisoned");
            let values = versions.entry(id.clone()).or_default();
            values.push(value.with_exposed(|bytes| bytes.to_vec()));
            values.len() as u64
        }
    }

    fn duration_ms(duration: Duration) -> u64 {
        duration.as_millis().min(u128::from(u64::MAX)) as u64
    }

    #[async_trait]
    impl crate::secrets::SecretProvider for RecordingSecretProvider {
        async fn get(
            &self,
            id: &crate::secrets::SecretId,
        ) -> Result<crate::secrets::SecretBytes, crate::secrets::SecretError> {
            self.read_latest(id)
                .map(|(_, value)| crate::secrets::SecretBytes::from(value))
        }

        async fn put(
            &self,
            id: &crate::secrets::SecretId,
            value: crate::secrets::SecretBytes,
        ) -> Result<(), crate::secrets::SecretError> {
            self.write_version(id, &value);
            Ok(())
        }

        async fn rotate(
            &self,
            id: &crate::secrets::SecretId,
        ) -> Result<crate::secrets::RotationHandle, crate::secrets::SecretError> {
            let (from_version, value) = self.read_latest(id)?;
            let to_version =
                self.write_version(id, &crate::secrets::SecretBytes::from(value.as_slice()));
            Ok(crate::secrets::RotationHandle {
                provider: self.namespace().to_string(),
                id: id
                    .clone()
                    .with_version(crate::secrets::SecretVersion::Exact(to_version)),
                from_version: Some(from_version),
                to_version: Some(to_version),
            })
        }

        async fn list(
            &self,
            _prefix: &crate::secrets::SecretId,
        ) -> Result<Vec<crate::secrets::SecretMeta>, crate::secrets::SecretError> {
            Ok(Vec::new())
        }

        async fn read_scoped(
            &self,
            request: crate::secrets::SecretReadRequest,
        ) -> Result<crate::secrets::SecretBytes, crate::secrets::SecretError> {
            self.record(
                "read",
                &request.id,
                &request.scope,
                &request.audit,
                None,
                None,
                None,
            );
            self.read_latest(&request.id)
                .map(|(_, value)| crate::secrets::SecretBytes::from(value))
        }

        async fn write_scoped(
            &self,
            request: crate::secrets::SecretWriteRequest,
        ) -> Result<crate::secrets::SecretWriteReceipt, crate::secrets::SecretError> {
            let ttl_ms = request.options.ttl.map(duration_ms);
            self.record(
                "write",
                &request.id,
                &request.scope,
                &request.audit,
                None,
                None,
                ttl_ms,
            );
            let version = self.write_version(&request.id, &request.value);
            Ok(crate::secrets::SecretWriteReceipt {
                provider: self.namespace().to_string(),
                id: request
                    .id
                    .with_version(crate::secrets::SecretVersion::Exact(version)),
                scope: request.scope,
                version: Some(version),
                expires_at_unix_ms: ttl_ms.map(|ttl| 1_700_000_000_000_i64 + ttl as i64),
            })
        }

        async fn rotate_scoped(
            &self,
            request: crate::secrets::SecretRotateRequest,
        ) -> Result<crate::secrets::SecretRotationReceipt, crate::secrets::SecretError> {
            let grace_ms = request.options.grace.map(duration_ms);
            let ttl_ms = request.options.ttl.map(duration_ms);
            self.record(
                "rotate",
                &request.id,
                &request.scope,
                &request.audit,
                None,
                grace_ms,
                ttl_ms,
            );
            let from_version = self
                .inner
                .versions
                .lock()
                .expect("versions lock poisoned")
                .get(&request.id)
                .map(|values| values.len() as u64);
            let to_version = self.write_version(&request.id, &request.value);
            Ok(crate::secrets::SecretRotationReceipt {
                provider: self.namespace().to_string(),
                id: request
                    .id
                    .with_version(crate::secrets::SecretVersion::Exact(to_version)),
                scope: request.scope,
                from_version,
                to_version: Some(to_version),
                grace_until_unix_ms: grace_ms.map(|grace| 1_700_000_000_000_i64 + grace as i64),
                expires_at_unix_ms: ttl_ms.map(|ttl| 1_700_000_000_000_i64 + ttl as i64),
            })
        }

        async fn lease_scoped(
            &self,
            request: crate::secrets::SecretLeaseRequest,
        ) -> Result<crate::secrets::SecretLeaseGrant, crate::secrets::SecretError> {
            let duration = duration_ms(request.duration);
            self.record(
                "lease",
                &request.id,
                &request.scope,
                &request.audit,
                Some(duration),
                None,
                None,
            );
            let (version, value) = self.read_latest(&request.id)?;
            Ok(crate::secrets::SecretLeaseGrant {
                provider: self.namespace().to_string(),
                id: request
                    .id
                    .with_version(crate::secrets::SecretVersion::Exact(version)),
                scope: request.scope,
                lease_id: format!("lease-{version}"),
                value: crate::secrets::SecretBytes::from(value),
                expires_at_unix_ms: 1_700_000_000_000_i64 + duration as i64,
            })
        }

        async fn delete_scoped(
            &self,
            request: crate::secrets::SecretDeleteRequest,
        ) -> Result<(), crate::secrets::SecretError> {
            self.record(
                "delete",
                &request.id,
                &request.scope,
                &request.audit,
                None,
                None,
                None,
            );
            let removed = self
                .inner
                .versions
                .lock()
                .expect("versions lock poisoned")
                .remove(&request.id)
                .is_some();
            if removed {
                Ok(())
            } else {
                Err(crate::secrets::SecretError::NotFound {
                    provider: self.namespace().to_string(),
                    id: request.id,
                })
            }
        }

        fn namespace(&self) -> &'static str {
            "recording"
        }

        fn supports_versions(&self) -> bool {
            true
        }
    }

    #[test]
    fn real_constructs_without_panic() {
        let _harness = Harness::real();
    }

    #[test]
    fn sub_handles_share_inner_state() {
        let harness = Harness::real();
        let stdio_inner = Arc::as_ptr(harness.stdio().inner());
        let clock_inner = Arc::as_ptr(harness.clock().inner());
        assert_eq!(stdio_inner, clock_inner, "sub-handles share Arc<Inner>");
    }

    #[test]
    fn kinds_round_trip_through_field_names() {
        for kind in HarnessKind::SUB_HANDLES {
            let field = kind.field_name().unwrap();
            assert_eq!(HarnessKind::from_field_name(field), Some(*kind));
        }
        assert!(HarnessKind::from_field_name("nope").is_none());
        assert!(HarnessKind::Root.field_name().is_none());
    }

    #[test]
    fn vm_harness_property_access_returns_sub_handle() {
        let root = match Harness::real().into_vm_value() {
            crate::value::VmValue::Harness(h) => h,
            other => panic!("expected Harness variant, got {}", other.type_name()),
        };
        let stdio = root.sub_handle("stdio").expect("stdio sub-handle");
        assert_eq!(stdio.kind(), HarnessKind::Stdio);
        assert!(stdio.sub_handle("clock").is_none(), "nested access denied");
        assert!(root.sub_handle("not_a_field").is_none());
    }

    #[test]
    fn narrowed_helper_cannot_recover_an_ungranted_sibling_capability() {
        let error = run_harness_source(
            r#"
fn fetch(fs: HarnessFs) {
  return fs.net.get("https://example.test")
}

fn main(harness: Harness) {
  fetch(harness.fs)
}
"#,
            Harness::real(),
        )
        .expect_err("HarnessFs must not expose HarnessNet authority");

        assert!(
            error.contains("cannot access property `net` on HarnessFs"),
            "unexpected attenuation error: {error}"
        );
    }

    #[test]
    fn test_constructor_clock_advances_under_paused_clock_advance() {
        let (harness, paused) = Harness::test();
        let clock = harness.clock();
        let start_wall = clock.clock().now_utc();
        assert_eq!(start_wall, OffsetDateTime::UNIX_EPOCH);
        assert_eq!(clock.clock().monotonic_ms(), 0);

        paused.advance(Duration::from_millis(1_500));
        assert_eq!(clock.clock().monotonic_ms(), 1_500);
        let after_wall = clock.clock().now_utc();
        assert_eq!(after_wall - start_wall, time::Duration::milliseconds(1_500));
    }

    #[test]
    fn with_paused_clock_pins_origin() {
        let origin = OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap();
        let (harness, paused) = Harness::with_paused_clock(origin);
        assert_eq!(harness.clock().clock().now_utc(), origin);
        paused.advance(Duration::from_mins(1));
        assert_eq!(
            harness.clock().clock().now_utc() - origin,
            time::Duration::seconds(60)
        );
    }

    #[test]
    fn null_harness_records_deny_events_for_every_sub_handle() {
        let harness = Harness::null();
        for source in [
            r#"fn main(harness: Harness) { harness.stdio.println("blocked") }"#,
            r"fn main(harness: Harness) { harness.term.width() }",
            r"fn main(harness: Harness) { harness.clock.now_ms() }",
            r#"fn main(harness: Harness) { harness.fs.read_text("/x") }"#,
            r#"fn main(harness: Harness) { harness.env.get("KEY") }"#,
            r"fn main(harness: Harness) { harness.random.u64() }",
            r#"fn main(harness: Harness) { harness.net.get("https://example.test") }"#,
            r#"fn main(harness: Harness) { harness.process.run({program: "printf", args: ["x"]}) }"#,
            r"fn main(harness: Harness) { harness.system.cpu() }",
            r#"fn main(harness: Harness) { harness.secrets.read("blocked") }"#,
            r"fn main(harness: Harness) { harness.llm.catalog() }",
            r"fn main(harness: Harness) { harness.tenant.id() }",
            r"fn main(harness: Harness) { harness.auth.subject() }",
            r#"fn main(harness: Harness) { harness.obs.log("blocked", "info", {}) }"#,
        ] {
            let error = run_harness_source(source, harness.clone()).expect_err("call denied");
            assert!(
                error.contains("NullHarness denied"),
                "unexpected deny error: {error}"
            );
        }

        let events = harness.deny_events();
        let observed: Vec<_> = events
            .iter()
            .map(|event| (event.sub_handle, event.method.as_str()))
            .collect();
        assert_eq!(
            observed,
            vec![
                (HarnessKind::Stdio, "println"),
                (HarnessKind::Term, "width"),
                (HarnessKind::Clock, "now_ms"),
                (HarnessKind::Fs, "read_text"),
                (HarnessKind::Env, "get"),
                (HarnessKind::Random, "u64"),
                (HarnessKind::Net, "get"),
                (HarnessKind::Process, "run"),
                (HarnessKind::System, "cpu"),
                (HarnessKind::Secrets, "read"),
                (HarnessKind::Llm, "catalog"),
                (HarnessKind::Tenant, "id"),
                (HarnessKind::Auth, "subject"),
                (HarnessKind::Obs, "log"),
            ]
        );
        assert_eq!(events[0].args, vec!["blocked"]);
        assert_eq!(events[3].args, vec!["/x"]);
    }

    #[test]
    fn auth_sub_handle_reads_bound_principal() {
        use crate::harness_auth::{enter_auth_principal, AuthPrincipal};
        let _principal = enter_auth_principal(AuthPrincipal {
            subject: "k_123".to_string(),
            scheme: "apikey".to_string(),
            scopes: ["admin:dlq:write", "read:events"]
                .iter()
                .map(|scope| scope.to_string())
                .collect(),
            kind: Some("operator".to_string()),
        });
        let source = r#"
fn main(harness: Harness) {
  harness.stdio.println(harness.auth.is_authenticated())
  harness.stdio.println(harness.auth.subject())
  harness.stdio.println(harness.auth.scheme())
  harness.stdio.println(harness.auth.kind())
  harness.stdio.println(harness.auth.has_scope("admin:dlq:write"))
  harness.stdio.println(harness.auth.has_scope("missing:scope"))
  harness.stdio.println(len(harness.auth.scopes()))
}
"#;
        let output = run_harness_source(source, Harness::real()).expect("dispatch succeeds");
        assert_eq!(output, "true\nk_123\napikey\noperator\ntrue\nfalse\n2\n");
    }

    #[test]
    fn auth_sub_handle_without_principal_reports_anonymous() {
        // No `enter_auth_principal` guard — the dispatch is unauthenticated,
        // so the presence/scope getters degrade rather than error and
        // `subject()` raises the canonical Auth error.
        let source = r#"
fn main(harness: Harness) {
  if harness.auth.is_authenticated() { harness.stdio.println("auth") } else { harness.stdio.println("anon") }
  harness.stdio.println(harness.auth.has_scope("x"))
  harness.stdio.println(len(harness.auth.scopes()))
}
"#;
        let output = run_harness_source(source, Harness::real()).expect("dispatch succeeds");
        assert_eq!(output, "anon\nfalse\n0\n");

        let error = run_harness_source(
            r"fn main(harness: Harness) { harness.auth.subject() }",
            Harness::real(),
        )
        .expect_err("subject() requires a bound principal");
        assert!(
            error.contains("no principal bound"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn secrets_sub_handle_uses_provider_scope_and_audit_context() {
        use crate::harness_auth::{enter_auth_principal, AuthPrincipal};
        use crate::harness_tenant::enter_tenant;
        use crate::observability::request_id::enter_request_id;

        let provider = RecordingSecretProvider::default();
        let harness = Harness::real().with_secret_provider(Arc::new(provider.clone()));
        let _tenant = enter_tenant(crate::TenantId::new("tenant-a"));
        let _request = enter_request_id("req-499");
        let _principal = enter_auth_principal(AuthPrincipal {
            subject: "api-key-1".to_string(),
            scheme: "apikey".to_string(),
            scopes: ["secrets:read", "secrets:write"]
                .iter()
                .map(|scope| scope.to_string())
                .collect(),
            kind: Some("tenant_api_key".to_string()),
        });

        let source = r#"
fn main(harness: Harness) {
  const scope = {kind: "workspace", id: "workspace-a"}
  const written = harness.secrets.write("github.token", "v1", scope, 5000)
  harness.stdio.println(written.provider)
  harness.stdio.println(written.scope.kind)
  harness.stdio.println(written.scope.id)
  harness.stdio.println(written.id.namespace)
  harness.stdio.println(written.version)
  harness.stdio.println(harness.secrets.read("github.token", scope))
  const rotated = harness.secrets.rotate("github.token", { -> "v2" }, scope, {grace_ms: 250, ttl_ms: 7500})
  harness.stdio.println(rotated.from_version)
  harness.stdio.println(rotated.to_version)
  const grant = harness.secrets.lease("github.token", 1000, scope)
  harness.stdio.println(grant.value)
  harness.stdio.println(grant.scope.id)
}
"#;
        let output = run_harness_source(source, harness).expect("dispatch succeeds");
        assert_eq!(
            output,
            "recording\nworkspace\nworkspace-a\nharn.workspace.workspace-a\n1\nv1\n1\n2\nv2\nworkspace-a\n"
        );

        let calls = provider.calls();
        assert_eq!(
            calls.iter().map(|call| call.operation).collect::<Vec<_>>(),
            vec!["write", "read", "rotate", "lease"]
        );
        for call in &calls {
            assert_eq!(
                call.scope,
                crate::secrets::SecretScope::workspace("workspace-a")
            );
            assert_eq!(call.request_id.as_deref(), Some("req-499"));
            assert_eq!(call.actor_subject.as_deref(), Some("api-key-1"));
            assert_eq!(call.actor_kind.as_deref(), Some("tenant_api_key"));
        }
        assert_eq!(calls[0].ttl_ms, Some(5_000));
        assert_eq!(calls[2].grace_ms, Some(250));
        assert_eq!(calls[2].ttl_ms, Some(7_500));
        assert_eq!(calls[3].duration_ms, Some(1_000));
    }

    #[test]
    fn secrets_sub_handle_accepts_absolute_connector_secret_ids() {
        let provider = RecordingSecretProvider::default();
        let harness = Harness::real().with_secret_provider(Arc::new(provider.clone()));

        let source = r#"
fn main(harness: Harness) {
  harness.secrets.write("google_workspace/access-token", "token-v1")
  harness.stdio.println(harness.secrets.read("google_workspace/access-token"))
  harness.secrets.write("harn-secret://google_workspace/refresh-token", "refresh-v1")
  harness.stdio.println(harness.secrets.read("harn-secret://google_workspace/refresh-token"))
}
"#;
        let output = run_harness_source(source, harness).expect("dispatch succeeds");
        assert_eq!(output, "token-v1\nrefresh-v1\n");

        let calls = provider.calls();
        assert_eq!(
            calls.iter().map(|call| call.id.clone()).collect::<Vec<_>>(),
            vec![
                crate::secrets::connector_access_token_id("google_workspace"),
                crate::secrets::connector_access_token_id("google_workspace"),
                crate::secrets::connector_refresh_token_id("google_workspace"),
                crate::secrets::connector_refresh_token_id("google_workspace"),
            ]
        );
    }

    // End-to-end fixture for the `std/oauth/storage` `secrets({provider})`
    // backend, driven entirely in-process against the fake secret host —
    // no network, no real keyring. Exercises roundtrip, rotating-refresh
    // preservation, delete, and `harn connect` composition (connector-shaped
    // scopes string + preserved connector metadata).
    #[test]
    fn secrets_backed_oauth_storage_roundtrips_and_preserves_refresh() {
        let provider = RecordingSecretProvider::default();
        let harness = Harness::real().with_secret_provider(Arc::new(provider.clone()));

        let source = r#"
import { secrets } from "std/oauth/storage"

fn assert_true(label: string, cond: bool) {
  if !cond { throw label }
}

fn main(harness: Harness) {
  const store = secrets(harness.auth, harness.secrets, {provider: "github"})

  // Absent key -> nil (NotFound is swallowed by the backend).
  assert_true("missing-nil", store.get("github") == nil)

  // Roundtrip: the client default storage_key is the provider id.
  store.set(
    "github",
    {access_token: "a1", refresh_token: "r1", expires_at_unix: 100, scopes: ["repo", "read:user"]},
  )
  const t1 = store.get("github")
  assert_true("t1-access", t1.access_token == "a1")
  assert_true("t1-refresh", t1.refresh_token == "r1")
  assert_true("t1-scopes", join(t1.scopes, ",") == "repo,read:user")

  // Update omitting the refresh token must NOT drop it.
  store.set("github", {access_token: "a2", expires_at_unix: 200})
  const t2 = store.get("github")
  assert_true("t2-access", t2.access_token == "a2")
  assert_true("t2-refresh-preserved", t2.refresh_token == "r1")

  // A newly-issued refresh token rotates the stored one.
  store.set("github", {access_token: "a3", refresh_token: "r2"})
  assert_true("t3-refresh-rotated", store.get("github").refresh_token == "r2")

  // Delete clears the credential.
  store.delete("github")
  assert_true("deleted-nil", store.get("github") == nil)

  // Compose with `harn connect`: seed the canonical <provider>/oauth-token
  // secret with a connector-shaped payload (scopes as a space-joined string
  // plus connector-only metadata).
  const seed = json_stringify(
    {
      access_token: "conn-a",
      refresh_token: "conn-r",
      scopes: "repo read:user",
      token_endpoint: "https://github.com/login/oauth/access_token",
      client_id: "cid",
    },
  )
  harness.secrets.write("github/oauth-token", seed)

  const c1 = store.get("github")
  assert_true("compose-access", c1.access_token == "conn-a")
  assert_true("compose-scopes-normalized", join(c1.scopes, ",") == "repo,read:user")

  // A refresh writing a fresh token must preserve the connector metadata.
  store.set("github", {access_token: "conn-a2", refresh_token: "conn-r2", expires_at_unix: 300})
  const reparsed = json_parse(harness.secrets.read("github/oauth-token"))
  assert_true("compose-access2", reparsed.access_token == "conn-a2")
  assert_true(
    "compose-endpoint-preserved",
    reparsed.token_endpoint == "https://github.com/login/oauth/access_token",
  )
  assert_true("compose-client-id-preserved", reparsed.client_id == "cid")

  harness.stdio.println("secrets-storage-ok")
}
"#;
        let output = run_harness_source(source, harness).expect("dispatch succeeds");
        assert_eq!(output, "secrets-storage-ok\n");

        // The token blob is stored under the connector's canonical id.
        assert!(
            provider
                .calls()
                .iter()
                .any(|call| call.id == crate::secrets::connector_oauth_token_id("github")),
            "expected writes under the canonical github/oauth-token id"
        );
    }

    #[test]
    fn secrets_sub_handle_denies_runtime_reserved_provenance_namespace() {
        let provider = RecordingSecretProvider::default();
        let harness = Harness::real().with_secret_provider(Arc::new(provider.clone()));

        for source in [
            r#"
fn main(harness: Harness) {
  harness.secrets.read("harn-cli.ed25519.seed", {kind: "provenance"})
}
"#,
            r#"
fn main(harness: Harness) {
  harness.secrets.write("harn-cli.ed25519.seed", "forged", {kind: "provenance"})
}
"#,
            r#"
fn main(harness: Harness) {
  harness.secrets.rotate("harn-cli.ed25519.seed", { -> "forged" }, {kind: "provenance"})
}
"#,
            r#"
fn main(harness: Harness) {
  harness.secrets.lease("harn-cli.ed25519.seed", 1000, {kind: "provenance"})
}
"#,
        ] {
            let error =
                run_harness_source(source, harness.clone()).expect_err("reserved secret denied");
            assert!(
                error.contains("reserved for Harn runtime provenance signing"),
                "unexpected error: {error}"
            );
        }

        assert!(
            provider.calls().is_empty(),
            "reserved namespace denial must happen before provider dispatch"
        );
    }

    #[test]
    fn mock_harness_replays_canned_responses_and_records_calls() {
        let harness = Harness::mock()
            .clock_at_unix_ms(1_700_000_000_000)
            .env("KEY", "value")
            .fs_read("/x", b"data".to_vec())
            .random_u64(42)
            .net_get("https://example.test", "body")
            .build();

        let output = run_harness_source(
            r#"
fn main(harness: Harness) {
  harness.stdio.print("partial ")
  harness.stdio.println("line")
  harness.stdio.println(harness.term.width())
  harness.stdio.println(harness.term.height())
  harness.stdio.println(harness.clock.now_ms())
  harness.clock.sleep_ms(250)
  harness.stdio.println(harness.clock.now_ms())
  harness.stdio.println(harness.clock.monotonic_ms())
  harness.stdio.println(harness.env.get("KEY"))
  harness.stdio.println(harness.fs.read_text("/x"))
  harness.stdio.println(harness.fs.exists("/missing"))
  harness.stdio.println(harness.random.u64())
  harness.stdio.println(harness.net.get("https://example.test"))
  harness.stdio.println(sha256_hex(""))
  harness.stdio.println(len(harness.llm.catalog()) > 0)
}
"#,
            harness.clone(),
        )
        .expect("mock harness run succeeds");

        assert_eq!(output, "");
        assert_eq!(
            harness.captured_stdio(),
            "partial line\n\
80\n24\n1700000000000\n1700000000250\n250\nvalue\ndata\nfalse\n42\nbody\n\
e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855\ntrue\n"
        );
        let observed: Vec<_> = harness
            .calls()
            .into_iter()
            .map(|call| (call.sub_handle, call.method))
            .collect();
        assert_eq!(
            observed,
            vec![
                (HarnessKind::Stdio, "print".to_string()),
                (HarnessKind::Stdio, "println".to_string()),
                (HarnessKind::Term, "width".to_string()),
                (HarnessKind::Stdio, "println".to_string()),
                (HarnessKind::Term, "height".to_string()),
                (HarnessKind::Stdio, "println".to_string()),
                (HarnessKind::Clock, "now_ms".to_string()),
                (HarnessKind::Stdio, "println".to_string()),
                (HarnessKind::Clock, "sleep_ms".to_string()),
                (HarnessKind::Clock, "now_ms".to_string()),
                (HarnessKind::Stdio, "println".to_string()),
                (HarnessKind::Clock, "monotonic_ms".to_string()),
                (HarnessKind::Stdio, "println".to_string()),
                (HarnessKind::Env, "get".to_string()),
                (HarnessKind::Stdio, "println".to_string()),
                (HarnessKind::Fs, "read_text".to_string()),
                (HarnessKind::Stdio, "println".to_string()),
                (HarnessKind::Fs, "exists".to_string()),
                (HarnessKind::Stdio, "println".to_string()),
                (HarnessKind::Random, "u64".to_string()),
                (HarnessKind::Stdio, "println".to_string()),
                (HarnessKind::Net, "get".to_string()),
                (HarnessKind::Stdio, "println".to_string()),
                (HarnessKind::Stdio, "println".to_string()),
                (HarnessKind::Llm, "catalog".to_string()),
                (HarnessKind::Stdio, "println".to_string()),
            ]
        );
    }

    #[test]
    fn mock_harness_owns_generic_capability_responses_without_global_registries() {
        let harness = Harness::mock()
            .capability_response(
                harn_builtin_meta::CapabilityId::Process,
                "run",
                crate::stdlib::json_to_vm_value(&serde_json::json!({"ok": true})),
            )
            .capability_response(
                harn_builtin_meta::CapabilityId::Interaction,
                "ask",
                crate::VmValue::String(arcstr::ArcStr::from("yes")),
            )
            .capability_response(
                harn_builtin_meta::CapabilityId::Project,
                "scan",
                crate::stdlib::json_to_vm_value(&serde_json::json!({"language": "rust"})),
            )
            .capability_response(
                harn_builtin_meta::CapabilityId::Workspace,
                "search",
                crate::stdlib::json_to_vm_value(&serde_json::json!({"matches": ["src/lib.rs"]})),
            )
            .build();

        run_harness_source(
            r#"
pipeline default(harness: Harness) {
  const process = harness.process.run({program: "never-executed"})
  const answer = harness.interaction.ask("continue?")
  const project = harness.project.scan("/never-read")
  const search = harness.workspace.search({query: "Harness"})
  harness.stdio.println(process.ok)
  harness.stdio.println(answer)
  harness.stdio.println(project.language)
  harness.stdio.println(search.matches[0])
}
"#,
            harness.clone(),
        )
        .expect("every effect is served by the harness instance");

        assert_eq!(
            harness.captured_stdio(),
            "true\nyes\nrust\nsrc/lib.rs\n"
        );
        let observed = harness
            .calls()
            .into_iter()
            .map(|call| (call.sub_handle, call.method))
            .collect::<Vec<_>>();
        assert_eq!(
            observed,
            vec![
                (HarnessKind::Process, "run".to_string()),
                (HarnessKind::Interaction, "ask".to_string()),
                (HarnessKind::Project, "scan".to_string()),
                (HarnessKind::Workspace, "search".to_string()),
                (HarnessKind::Stdio, "println".to_string()),
                (HarnessKind::Stdio, "println".to_string()),
                (HarnessKind::Stdio, "println".to_string()),
                (HarnessKind::Stdio, "println".to_string()),
            ]
        );
    }

    #[test]
    fn optional_typed_capability_dispatches_through_the_embedder_bridge() {
        let bridge = Arc::new(RecordingWorkspaceBridge::default());
        crate::stdlib::host::set_host_call_bridge(bridge.clone());
        let _guard = HostBridgeGuard;

        let output = run_harness_source(
            r#"
fn main(harness: Harness) {
  const result = harness.workspace.search({query: "runtime"})
  harness.stdio.println(result.matches[0])
}
"#,
            Harness::real(),
        )
        .expect("typed optional capability reaches the host bridge");

        assert_eq!(output, "src/runtime.rs\n");
        let calls = bridge.calls.lock().expect("calls poisoned");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "workspace");
        assert_eq!(calls[0].1, "search");
        assert_eq!(
            calls[0].2.get("query").map(VmValue::display).as_deref(),
            Some("runtime")
        );
    }

    #[test]
    fn mock_harness_records_repeated_cached_harness_method_calls() {
        let harness = Harness::mock().env("KEY", "value").build();

        run_harness_source(
            r#"
fn main(harness: Harness) {
  let i = 0
  while i < 3 {
    const _ = harness.clock.elapsed()
    const value = harness.env.get_or("KEY", "")
    harness.stdio.println(value)
    i = i + 1
  }
}
"#,
            harness.clone(),
        )
        .expect("mock harness run succeeds");

        assert_eq!(harness.captured_stdio(), "value\nvalue\nvalue\n");
        let observed: Vec<_> = harness
            .calls()
            .into_iter()
            .map(|call| (call.sub_handle, call.method))
            .collect();
        assert_eq!(
            observed,
            vec![
                (HarnessKind::Clock, "elapsed".to_string()),
                (HarnessKind::Env, "get_or".to_string()),
                (HarnessKind::Stdio, "println".to_string()),
                (HarnessKind::Clock, "elapsed".to_string()),
                (HarnessKind::Env, "get_or".to_string()),
                (HarnessKind::Stdio, "println".to_string()),
                (HarnessKind::Clock, "elapsed".to_string()),
                (HarnessKind::Env, "get_or".to_string()),
                (HarnessKind::Stdio, "println".to_string()),
            ]
        );
    }

    #[test]
    fn mock_harness_replays_random_values_fifo() {
        let harness = Harness::mock()
            .random_u64(7)
            .random_u64(11)
            .random_u64(u64::MAX)
            .build();

        let output = run_harness_source(
            r"
fn main(harness: Harness) {
  harness.stdio.println(harness.random.u64())
  harness.stdio.println(harness.random.u64())
  harness.stdio.println(harness.random.u64())
}
",
            harness.clone(),
        )
        .expect("mock random succeeds");

        assert_eq!(output, "");
        assert_eq!(
            harness.captured_stdio(),
            "7\n11\n9223372036854775807\n"
        );
    }

    #[test]
    fn mock_harness_reports_missing_canned_responses() {
        let cases = [
            (
                r#"fn main(harness: Harness) { harness.fs.read_text("/missing") }"#,
                "MockHarness has no fs_read response for /missing",
            ),
            (
                r"fn main(harness: Harness) { harness.random.u64() }",
                "MockHarness has no random_u64 response",
            ),
            (
                r#"fn main(harness: Harness) { harness.net.get("https://missing.test") }"#,
                "MockHarness has no net_get response for https://missing.test",
            ),
            (
                r#"fn main(harness: Harness) { harness.process.run({program: "printf", args: ["x"]}) }"#,
                "MockHarness has no process response",
            ),
        ];

        for (source, expected) in cases {
            let error = run_harness_source(source, Harness::mock().build())
                .expect_err("missing mock response fails");
            assert!(
                error.contains(expected),
                "expected `{expected}` in `{error}`"
            );
        }
    }

    #[test]
    fn mock_harness_records_failed_calls() {
        let harness = Harness::mock().build();
        let error = run_harness_source(
            r#"fn main(harness: Harness) { harness.net.get("https://missing.test") }"#,
            harness.clone(),
        )
        .expect_err("missing mock response fails");

        assert!(error.contains("MockHarness has no net_get response"));
        assert_eq!(
            harness.calls(),
            vec![HarnessCall {
                sub_handle: HarnessKind::Net,
                method: "get".to_string(),
                args: vec!["https://missing.test".to_string()],
            }]
        );
    }

    #[test]
    fn mock_harness_captures_stderr_separately_from_stdout() {
        let harness = Harness::mock().build();
        run_harness_source(
            r#"
fn main(harness: Harness) {
  harness.stdio.println("stdout line")
  harness.stdio.eprint("err ")
  harness.stdio.eprintln("trail")
}
"#,
            harness.clone(),
        )
        .expect("stderr capture run succeeds");
        assert_eq!(harness.captured_stdio(), "stdout line\n");
        assert_eq!(harness.captured_stderr(), "err trail\n");
    }

    #[test]
    fn mock_harness_replays_stdin_lines_for_read_and_prompt() {
        let harness = Harness::mock()
            .stdin_line("first")
            .stdin_line("second")
            .build();
        let output = run_harness_source(
            r#"
fn main(harness: Harness) {
  harness.stdio.println(harness.stdio.read_line())
  harness.stdio.println(harness.stdio.prompt("answer: "))
  const eof = harness.stdio.read_line({trim: false})
  harness.stdio.println(eof.status)
}
"#,
            harness.clone(),
        )
        .expect("stdin replay succeeds");
        // All stdio writes route to the mock capture buffer; vm.output stays empty.
        assert_eq!(output, "");
        assert_eq!(harness.captured_stdio(), "first\nanswer: second\neof\n");
    }

    #[test]
    fn mock_harness_replays_password_input_without_stdout_echo() {
        let harness = Harness::mock().stdin_line("secret").build();
        let output = run_harness_source(
            r#"
fn main(harness: Harness) {
  harness.stdio.println(harness.term.read_password("password: "))
}
"#,
            harness.clone(),
        )
        .expect("stdin replay succeeds");

        assert_eq!(output, "");
        assert_eq!(harness.captured_stdio(), "secret\n");
        assert_eq!(harness.captured_stderr(), "password: ");
        assert_eq!(
            harness.calls(),
            vec![HarnessCall {
                sub_handle: HarnessKind::Term,
                method: "read_password".to_string(),
                args: vec!["password: ".to_string()],
            },
            HarnessCall {
                sub_handle: HarnessKind::Stdio,
                method: "println".to_string(),
                args: vec!["secret".to_string()],
            }]
        );
    }

    #[test]
    fn mock_harness_rejects_wrong_argument_types() {
        let error = run_harness_source(
            r"fn main(harness: Harness) { harness.fs.read_text(1) }",
            Harness::mock().build(),
        )
        .expect_err("wrong argument type fails");

        // A wrong-typed argument is rejected by one of two layers, and which
        // one fires depends on whether the process-global builtin-signature
        // registry was already populated (by any prior `register_vm_stdlib`
        // call in the test binary) when `compile_source` ran:
        //   - empty registry  -> the harness runtime guard (`string_arg`)
        //   - populated registry -> static type-check at compile time, which
        //     matches the `read_text` method against the same-named stdlib
        //     `read_text(path: string)` signature.
        // Both correctly reject the int, so accept either message.
        let runtime_rejection =
            error.contains("HarnessFs.read_text expects string argument 1, got int");
        let static_rejection = error.contains("argument 1 `path`: expected string, found int");
        assert!(
            runtime_rejection || static_rejection,
            "expected a string/int type rejection for read_text, got: {error}"
        );
    }

    #[test]
    fn real_harness_fs_write_outside_workspace_roots_surfaces_cap_201() {
        use crate::orchestration::{
            clear_execution_policy_stacks, push_execution_policy, CapabilityPolicy, SandboxProfile,
        };
        clear_execution_policy_stacks();
        let temp = tempfile::tempdir().unwrap();
        let policy = CapabilityPolicy {
            sandbox_profile: SandboxProfile::Worktree,
            workspace_roots: vec![temp.path().to_string_lossy().into_owned()],
            ..CapabilityPolicy::default()
        };
        push_execution_policy(policy);
        let outside = std::env::temp_dir().join("harn_e4_4_cap_201_outside.txt");
        let source = format!(
            r#"fn main(harness: Harness) {{ harness.fs.write_text("{}", "x") }}"#,
            outside.to_string_lossy().replace('\\', "/"),
        );
        let error = run_harness_source(&source, Harness::real())
            .expect_err("write outside workspace_roots must reject");
        clear_execution_policy_stacks();
        assert!(
            error.contains("HARN-CAP-201"),
            "expected HARN-CAP-201 prefix, got: {error}"
        );
        assert!(
            error.contains("sandbox violation"),
            "deny should keep the underlying sandbox-rejection message, got: {error}"
        );
    }

    #[test]
    fn runtime_effect_receipt_comes_from_the_capability_contract() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let effects = rt.block_on(async {
            let source = r#"fn main(harness: Harness) { harness.stdio.println("record me") }"#;
            harn_builtin_registry::install_builtin_manifest(crate::stdlib::all_builtin_manifest());
            let chunk = crate::compile_source(source).expect("compile");
            let mut vm = crate::Vm::new();
            crate::stdlib::register_vm_stdlib(&mut vm);
            vm.set_harness(Harness::mock().build());
            vm.execute(&chunk).await.expect("execute");
            vm.executed_effects()
        });

        assert_eq!(
            effects,
            vec![crate::orchestration::EffectRecord::new(
                crate::orchestration::EffectKind::Stdio,
                crate::orchestration::EffectScope::Write,
            )
            .with_resource("stdout")]
        );
    }

    fn run_harness_source(source: &str, harness: Harness) -> Result<String, String> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let local = tokio::task::LocalSet::new();
            local
                .run_until(async move {
                    let chunk = crate::compile_source(source)?;
                    let mut vm = crate::Vm::new();
                    crate::stdlib::register_vm_stdlib(&mut vm);
                    vm.set_harness(harness);
                    vm.execute(&chunk)
                        .await
                        .map_err(|error| error.to_string())?;
                    Ok(vm.output().to_string())
                })
                .await
        })
    }
}
