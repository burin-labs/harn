use super::*;

fn is_running(pid: u32) -> bool {
    let output = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .expect("read process liveness");
    assert!(output.status.success() || output.status.code() == Some(1));
    let status = String::from_utf8(output.stdout).unwrap();
    let status = status.trim();
    !status.is_empty() && !status.starts_with('Z')
}

struct CleanupSessions(Vec<String>);

impl Drop for CleanupSessions {
    fn drop(&mut self) {
        for session in &self.0 {
            harn_hostlib::tools::long_running::cancel_session_handles(session);
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn disconnected_and_shutdown_transports_reap_only_their_own_background_commands() {
    use harn_hostlib::tools::long_running::{register_result_notifier, spawn_long_running};
    use harn_vm::VmValue;

    let _guardian_args = harn_hostlib::process::owner_death::install_guardian_reexec_args([
        "--exact",
        "adapters::acp::tests::stop_controls::stop_controls_owner_death_guardian_fixture",
        "--nocapture",
    ]);
    let _environment = harn_vm::stdlib::process::declare_session_environment_if_absent(
        harn_vm::security::SessionEnvironment::inherited(),
    );
    harn_clock::test_support::with_local_tasks(async {
        for shutdown in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let (tx, rx) = mpsc::unbounded_channel();
            let (responses, mut messages) = mpsc::unbounded_channel();
            let (future, handle) = super::super::run_acp_channel_server_with_handle(
                AcpServerConfig::new(None), rx, responses,
            );
            let server = tokio::task::spawn_local(future);
            tx.send(serde_json::json!({
                "jsonrpc": "2.0", "id": 1, "method": "session/new",
                "params": {"cwd": dir.path(), "environmentPolicy": {"kind": "isolated", "grants": []}},
            })).unwrap();
            let created = recv_json(&mut messages).await;
            let owned = created["result"]["sessionId"].as_str().expect("created session").to_string();
            let outsider = format!("transport-bystander-{}", uuid::Uuid::new_v4());
            let _cleanup = CleanupSessions(vec![owned.clone(), outsider.clone()]);
            let spawn = |session: &str| {
                spawn_long_running(
                    "run", "sleep".to_string(), vec!["300".to_string()],
                    Some(dir.path().to_path_buf()), std::collections::BTreeMap::new(), session.to_string(),
                ).unwrap()
            };
            let child = spawn(&owned);
            let bystander = spawn(&outsider);
            let result = register_result_notifier(&child.handle_id).expect("registered live child");
            assert!(is_running(child.pid));
            assert!(is_running(bystander.pid));

            if shutdown {
                handle.shutdown();
            } else {
                drop(tx);
            }
            server.await.unwrap();
            assert!(handle.is_terminated());
            assert!(!is_running(child.pid), "transport-owned child survived termination");
            assert!(is_running(bystander.pid), "another transport's child was killed");
            let result = tokio::task::spawn_blocking(move || {
                harn_clock::test_support::recv_within("transport child cleanup receipt", &result)
            }).await.unwrap();
            let VmValue::Dict(result) = result else { panic!("expected command receipt") };
            let Some(VmValue::Dict(cleanup)) = result.get("process_cleanup") else {
                panic!("missing process cleanup observation: {result:?}");
            };
            let Some(VmValue::Int(root)) = cleanup.get("root_pid") else {
                panic!("cleanup omitted the owned process identity: {cleanup:?}");
            };
            assert!(*root > 0 && !is_running(*root as u32), "owned process survived cleanup: {cleanup:?}");
            let qualified = matches!(cleanup.get("census_complete"), Some(VmValue::Bool(true)))
                || matches!(cleanup.get("guardian_custody_complete"), Some(VmValue::Bool(true)));
            match cleanup.get("survivor_count") {
                Some(VmValue::Int(0)) => assert!(qualified, "zero survivors requires a complete observation: {cleanup:?}"),
                Some(VmValue::Nil) => assert!(!qualified, "an unreadable census must stay unconfirmed: {cleanup:?}"),
                other => panic!("cleanup reported a surviving process: {other:?}; {cleanup:?}"),
            }
        }
    }).await;
}
