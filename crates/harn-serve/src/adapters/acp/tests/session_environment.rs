//! The `session/new` environment policy is declared, never inferred.

use super::*;

pub(super) const RESTORE_CANARY: &str = "HARN_RESTORE_TEST_API_KEY";

/// Use the production process-construction owner. Print only a presence
/// verdict, never the synthetic value or any launcher credential.
pub(super) fn child_sees_restore_canary(
    environment: &harn_vm::security::SessionEnvironment,
) -> bool {
    struct Restore(Option<harn_vm::security::SessionEnvironment>);
    impl Drop for Restore {
        fn drop(&mut self) {
            harn_vm::stdlib::process::set_session_environment(self.0.take());
        }
    }
    let _restore = Restore(harn_vm::stdlib::process::current_session_environment());
    harn_vm::stdlib::process::set_session_environment(Some(environment.clone()));
    let mut command = if cfg!(windows) {
        let mut command = harn_vm::process_sandbox::session_std_command(
            std::env::var_os("COMSPEC").unwrap_or_else(|| "cmd.exe".into()),
        )
        .unwrap();
        command.args([
            "/C",
            "if defined HARN_RESTORE_TEST_API_KEY (echo visible) else (echo absent)",
        ]);
        command
    } else {
        let mut command = harn_vm::process_sandbox::session_std_command("/bin/sh").unwrap();
        command.args(["-c", "if [ -n \"${HARN_RESTORE_TEST_API_KEY-}\" ]; then printf visible; else printf absent; fi"]);
        command
    };
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "the calibrated child process must execute"
    );
    match String::from_utf8(output.stdout).unwrap().trim() {
        "visible" => true,
        "absent" => false,
        _ => panic!("child must report a non-null canary presence verdict"),
    }
}

/// harn#8566. A `session/new` that names no environment policy is refused,
/// and the refusal is usable on its own: it names the field and every value
/// the field accepts.
///
/// The defect this closes was not that the default was wrong. It was that a
/// client which declared nothing got `inherited` one day and `isolated` the
/// next, with no signal at either end. A refusal is what makes that state
/// unreachable, so the assertion to protect is that omission cannot open a
/// session at all, whatever this server's default would have been.
#[tokio::test(flavor = "current_thread")]
async fn session_new_refuses_an_omitted_environment_policy_and_names_its_values() {
    harn_vm::reset_thread_local_state();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut server = AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "session/new",
            "params": {"cwd": "."},
        }))
        .await;

    let refused = recv_json(&mut rx).await;
    assert!(
        refused["result"].is_null(),
        "an omitted policy must not open a session, got {refused}"
    );
    assert_eq!(refused["error"]["code"], serde_json::json!(-32602));
    let data = &refused["error"]["data"];
    assert_eq!(
        data["code"],
        serde_json::json!("environment_policy.missing")
    );
    assert_eq!(data["field"], serde_json::json!("environmentPolicy"));
    assert_eq!(
        data["accepted"],
        serde_json::json!(["inherited", "isolated", "granted"]),
        "the refusal must name every value the field accepts"
    );
    let message = refused["error"]["message"].as_str().expect("message");
    for kind in ["inherited", "isolated", "granted"] {
        assert!(
            message.contains(kind),
            "message must name {kind}: {message}"
        );
    }

    // The positive control, in the same test so the refusal cannot be a server
    // that refuses everything: a request that states the policy still opens.
    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/new",
            "params": {"cwd": ".", "environmentPolicy": {"kind": "isolated", "grants": []}},
        }))
        .await;
    let created = recv_json(&mut rx).await;
    assert!(
        created["result"]["sessionId"].as_str().is_some(),
        "a stated policy must still open a session, got {created}"
    );
}

/// A canonical history record cannot grant launcher credentials to a new
/// process. The cold admission must be explicit; a live replay cannot widen it.
#[test]
fn cold_load_requires_authority_and_live_load_cannot_replace_it() {
    use harn_session_store::{CreateSession, SessionStore};

    let _lock = acp_env_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let _environment = EnvSnapshot::capture(&[RESTORE_CANARY]);
    std::env::set_var(RESTORE_CANARY, "synthetic-not-a-credential");
    // Acquire the process environment guard before entering the executor.
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime")
        .block_on(async {
            harn_vm::reset_thread_local_state();
            let root = tempfile::tempdir().unwrap();
            let store = harn_vm::open_canonical_store(root.path()).unwrap();
            store
                .create(CreateSession {
                    id: Some("cold-authority".into()),
                    cwd: Some(root.path().to_string_lossy().into_owned()),
                    project_scope: Some(root.path().to_string_lossy().into_owned()),
                    ..CreateSession::default()
                })
                .await
                .unwrap();
            let (tx, mut rx) = mpsc::unbounded_channel();
            let mut server =
                AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));
            // Non-null calibration through the same launch resolver: inherited really
            // exposes the synthetic sensitive name, so an isolated zero is meaningful.
            let inherited = AcpServer::resolve_session_environment(&serde_json::json!({
                "environmentPolicy": {"kind": "inherited", "grants": []},
            }))
            .unwrap();
            assert!(child_sees_restore_canary(&inherited));

            for (id, policy, code) in [
                (1, None, "environment_policy.missing"),
                (
                    2,
                    Some(serde_json::Value::Null),
                    "environment_policy.invalid",
                ),
            ] {
                let mut params =
                    serde_json::json!({"sessionId": "cold-authority", "cwd": root.path()});
                if let Some(policy) = policy {
                    params["environmentPolicy"] = policy;
                }
                server
                    .handle_session_load(&serde_json::json!(id), &params)
                    .await;
                let refused = recv_json(&mut rx).await;
                assert_eq!(refused["error"]["data"]["code"], code);
                assert!(!server.sessions.contains_key("cold-authority"));
                assert!(!harn_vm::agent_sessions::exists("cold-authority"));
            }
            server
                .handle_session_load(
                    &serde_json::json!(3),
                    &serde_json::json!({
                        "sessionId": "cold-authority", "cwd": root.path(),
                        "environmentPolicy": {"kind": "isolated", "grants": []},
                    }),
                )
                .await;
            let loaded = recv_json(&mut rx).await;
            assert_eq!(loaded["result"]["sessionId"], "cold-authority");
            let admitted = server.sessions["cold-authority"].environment_policy.clone();
            assert!(admitted.is_isolated());
            assert!(!child_sees_restore_canary(&admitted));
            // Discard load replay notifications before the next response.
            while rx.try_recv().is_ok() {}

            for (id, policy, code) in [
                (
                    4,
                    serde_json::json!({"kind": "inherited", "grants": []}),
                    "environment_policy.live_mismatch",
                ),
                (5, serde_json::Value::Null, "environment_policy.invalid"),
            ] {
                server
            .handle_session_load(
                &serde_json::json!(id),
                &serde_json::json!({
                    "sessionId": "cold-authority", "cwd": root.path(), "environmentPolicy": policy,
                }),
            )
            .await;
                let refused = recv_json(&mut rx).await;
                assert_eq!(refused["error"]["data"]["code"], code);
                assert_eq!(
                    server.sessions["cold-authority"].environment_policy,
                    admitted
                );
            }
            server
                .handle_session_load(
                    &serde_json::json!(6),
                    &serde_json::json!({
                        "sessionId": "cold-authority", "cwd": root.path(),
                    }),
                )
                .await;
            let replayed = recv_json(&mut rx).await;
            assert_eq!(replayed["result"]["sessionId"], "cold-authority");
            assert_eq!(
                server.sessions["cold-authority"].environment_policy,
                admitted
            );
        });
}
