use super::*;

#[cfg(feature = "hostlib")]
#[tokio::test(flavor = "current_thread")]
async fn cold_loaded_session_captures_its_next_prompt() {
    harn_clock::test_support::with_local_tasks(async {
        use harn_session_store::{CreateSession, SessionStore, SqliteSessionStore};
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let git = |args: &[&str]| {
            assert!(std::process::Command::new("git").arg("-C").arg(&root).args(args).status().unwrap().success());
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@example.com"]);
        git(&["config", "user.name", "t"]);
        let pipeline = root.join("test.harn");
        std::fs::write(&pipeline, "pipeline default(harness: Harness) { 42 }\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "init"]);
        std::fs::create_dir(root.join(".harn")).unwrap();
        let session = "loaded-checkpoint-session";
        let store = SqliteSessionStore::open(root.join(".harn/session-store.sqlite")).unwrap();
        store.create(CreateSession {
            id: Some(session.to_string()),
            cwd: Some(root.display().to_string()),
            project_scope: Some(root.display().to_string()),
            ..CreateSession::default()
        }).await.unwrap();
        drop(store);

        let (request_tx, request_rx) = mpsc::unbounded_channel();
        let (response_tx, mut response_rx) = mpsc::unbounded_channel();
        let config = AcpServerConfig::for_pipeline(pipeline.to_string_lossy().into_owned());
        let _server = tokio::task::spawn_local(super::super::run_acp_channel_server(config, request_rx, response_tx));
        request_tx.send(serde_json::json!({
            "jsonrpc":"2.0", "id":1, "method":"session/load",
            "params":{"sessionId":session, "cwd":root.to_string_lossy(), "environmentPolicy":{"kind":"isolated", "grants":[]}, "workspaceCheckpoint":true},
        })).unwrap();
        let loaded = recv_response_with_id(&mut response_rx, 1).await;
        assert_eq!(loaded["result"]["workspaceCheckpointTracking"], true, "{loaded}");
        request_tx.send(serde_json::json!({
            "jsonrpc":"2.0", "id":2, "method":"session/prompt",
            "params":{"sessionId":session, "messageId":"after-load", "prompt":[{"type":"text", "text":"hello"}]},
        })).unwrap();
        let response = harn_clock::test_support::within("loaded prompt response", async {
            loop {
                let message = recv_json(&mut response_rx).await;
                if message["method"] == "host/capabilities" {
                    reply_empty_result(&request_tx, &message);
                } else if message["id"] == 2 {
                    break message;
                }
            }
        }).await;
        assert!(response["error"].is_null(), "{response}");
        let plan = harn_hostlib::workspace_checkpoint::plan(session, "after-load").unwrap();
        assert!(matches!(plan, harn_hostlib::workspace_checkpoint::Outcome::Ready(_)), "{plan:?}");
        harn_hostlib::workspace_checkpoint::drop_session(session);
    }).await;
}

#[cfg(feature = "hostlib")]
#[tokio::test(flavor = "current_thread")]
async fn opted_in_prompt_captures_its_caller_identity_at_admission() {
    harn_clock::test_support::with_local_tasks(async {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.email", "t@example.com"],
        vec!["config", "user.name", "t"],
    ] {
        assert!(std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .status()
            .unwrap()
            .success());
    }
    let pipeline = root.join("test.harn");
    std::fs::write(&pipeline, "pipeline default(harness: Harness) { 42 }\n").unwrap();
    assert!(std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["add", "-A"])
        .status()
        .unwrap()
        .success());
    assert!(std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["commit", "-q", "-m", "init"])
        .status()
        .unwrap()
        .success());

    let (request_tx, request_rx) = mpsc::unbounded_channel();
    let (response_tx, mut response_rx) = mpsc::unbounded_channel();
    let config = AcpServerConfig::for_pipeline(pipeline.to_string_lossy().into_owned());
    let _server = tokio::task::spawn_local(super::super::run_acp_channel_server(config, request_rx, response_tx));
    request_tx.send(serde_json::json!({
        "jsonrpc":"2.0", "id":1, "method":"session/new",
        "params":{"cwd":root.to_string_lossy(), "environmentPolicy":{"kind":"isolated", "grants":[]}, "workspaceCheckpoint":true},
    })).unwrap();
    let created = recv_response_with_id(&mut response_rx, 1).await;
        assert_eq!(created["result"]["workspaceCheckpointTracking"], true, "{created}");
    let session = created["result"]["sessionId"].as_str().unwrap().to_string();
    request_tx.send(serde_json::json!({
        "jsonrpc":"2.0", "id":20, "method":"session/prompt",
        "params":{"sessionId":session, "prompt":[{"type":"text", "text":"missing identity"}]},
    })).unwrap();
    let (missing, reached_host) = harn_clock::test_support::within("missing identity response", async {
        let mut reached_host = false;
        loop {
            let message = recv_json(&mut response_rx).await;
            if message["method"] == "host/capabilities" {
                reached_host = true;
                reply_empty_result(&request_tx, &message);
            } else if message["id"] == 20 {
                break (message, reached_host);
            }
        }
    }).await;
    assert!(missing["error"]["message"].as_str().unwrap_or_default().contains("messageId"), "{missing}");
    assert!(!reached_host, "missing identity reached the host pipeline");
    std::fs::remove_file(&pipeline).unwrap();
    request_tx.send(serde_json::json!({
        "jsonrpc":"2.0", "id":21, "method":"session/prompt",
        "params":{"sessionId":session, "messageId":"caller-turn", "prompt":[{"type":"text", "text":"retry"}]},
    })).unwrap();
    let rejected = recv_response_with_id(&mut response_rx, 21).await;
    assert!(rejected["error"]["message"].as_str().unwrap_or_default().contains("Failed to read pipeline"), "{rejected}");
    std::fs::write(&pipeline, "pipeline default(harness: Harness) { 42 }\n").unwrap();
    request_tx.send(serde_json::json!({
        "jsonrpc":"2.0", "id":2, "method":"session/prompt",
        "params":{"sessionId":session, "messageId":"caller-turn", "prompt":[{"type":"text", "text":"hello"}]},
    })).unwrap();
    let response = harn_clock::test_support::within("prompt response", async {
        loop {
            let message = recv_json(&mut response_rx).await;
            if message["method"] == "host/capabilities" {
                reply_empty_result(&request_tx, &message);
            } else if message["id"] == 2 {
                break message;
            }
        }
    }).await;
    assert!(response["error"].is_null(), "{response}");
    let plan = harn_hostlib::workspace_checkpoint::plan(&session, "caller-turn").unwrap();
    assert!(
        matches!(plan, harn_hostlib::workspace_checkpoint::Outcome::Ready(_)),
        "{plan:?}"
    );
        request_tx.send(serde_json::json!({
            "jsonrpc":"2.0", "id":13, "method":"session/fork",
            "params":{"sessionId":session, "inheritWorkspaceCheckpoint":true},
        })).unwrap();
        let fork = recv_response_with_id(&mut response_rx, 13).await;
        assert_eq!(fork["result"]["workspaceCheckpoint"]["available"], true, "{fork}");
        let child = fork["result"]["sessionId"].as_str().unwrap().to_string();
        request_tx.send(serde_json::json!({
            "jsonrpc":"2.0", "id":14, "method":"session/prompt",
            "params":{"sessionId":child, "messageId":"child-turn", "prompt":[{"type":"text", "text":"continue"}]},
        })).unwrap();
        let child_response = harn_clock::test_support::within("fork prompt response", async {
            loop {
                let message = recv_json(&mut response_rx).await;
                if message["method"] == "host/capabilities" {
                    reply_empty_result(&request_tx, &message);
                } else if message["id"] == 14 {
                    break message;
                }
            }
        }).await;
        assert!(child_response["error"].is_null(), "{child_response}");
        let child_plan = harn_hostlib::workspace_checkpoint::plan(&child, "child-turn").unwrap();
        assert!(matches!(child_plan, harn_hostlib::workspace_checkpoint::Outcome::Ready(_)), "{child_plan:?}");
        request_tx.send(serde_json::json!({
            "jsonrpc":"2.0", "id":3, "method":"session/prompt",
            "params":{"sessionId":session, "messageId":"caller-turn", "prompt":[{"type":"text", "text":"different turn"}]},
        })).unwrap();
        let duplicate = recv_response_with_id(&mut response_rx, 3).await;
        assert!(duplicate["error"]["message"].as_str().unwrap_or_default().contains("already used"), "{duplicate}");
        request_tx.send(serde_json::json!({
            "jsonrpc":"2.0", "id":4, "method":"session/new",
            "params":{"cwd":root.to_string_lossy(), "environmentPolicy":{"kind":"isolated", "grants":[]}},
        })).unwrap();
        let created = recv_response_with_id(&mut response_rx, 4).await;
        assert_eq!(created["result"]["workspaceCheckpointTracking"], false, "{created}");
        let untracked = created["result"]["sessionId"].as_str().unwrap().to_string();
        request_tx.send(serde_json::json!({
            "jsonrpc":"2.0", "id":5, "method":"session/prompt",
            "params":{"sessionId":untracked, "messageId":"ordinary-turn", "prompt":[{"type":"text", "text":"hello"}]},
        })).unwrap();
        let ordinary = harn_clock::test_support::within("ordinary prompt response", async {
            loop {
                let message = recv_json(&mut response_rx).await;
                if message["method"] == "host/capabilities" {
                    reply_empty_result(&request_tx, &message);
                } else if message["id"] == 5 {
                    break message;
                }
            }
        }).await;
        assert!(ordinary["error"].is_null(), "{ordinary}");
        request_tx.send(serde_json::json!({
            "jsonrpc":"2.0", "id":21, "method":"session/prompt",
            "params":{"sessionId":untracked, "prompt":[{"type":"text", "text":"untracked without identity"}]},
        })).unwrap();
        let ordinary_without_id = harn_clock::test_support::within("untracked prompt response", async {
            loop {
                let message = recv_json(&mut response_rx).await;
                if message["method"] == "host/capabilities" {
                    reply_empty_result(&request_tx, &message);
                } else if message["id"] == 21 {
                    break message;
                }
            }
        }).await;
        assert!(ordinary_without_id["error"].is_null(), "{ordinary_without_id}");
        let untracked_plan = harn_hostlib::workspace_checkpoint::plan(&untracked, "ordinary-turn").unwrap();
        assert!(matches!(untracked_plan, harn_hostlib::workspace_checkpoint::Outcome::Unavailable(_)), "{untracked_plan:?}");
        request_tx.send(serde_json::json!({
            "jsonrpc":"2.0", "id":6, "method":"session/new",
            "params":{"cwd":root.to_string_lossy(), "environmentPolicy":{"kind":"isolated", "grants":[]}, "workspaceCheckpoint":true},
        })).unwrap();
        let created = recv_response_with_id(&mut response_rx, 6).await;
        let collision = created["result"]["sessionId"].as_str().unwrap().to_string();
        let old = harn_hostlib::workspace_checkpoint::capture(&collision, "reused", Some(root)).unwrap();
        assert!(matches!(old, harn_hostlib::workspace_checkpoint::Outcome::Ready(_)), "{old:?}");
        request_tx.send(serde_json::json!({
            "jsonrpc":"2.0", "id":7, "method":"session/prompt",
            "params":{"sessionId":collision, "messageId":"reused", "prompt":[{"type":"text", "text":"hello"}]},
        })).unwrap();
        let response = harn_clock::test_support::within("collision prompt response", async {
            loop {
                let message = recv_json(&mut response_rx).await;
                if message["method"] == "host/capabilities" {
                    reply_empty_result(&request_tx, &message);
                } else if message["id"] == 7 {
                    break message;
                }
            }
        }).await;
        assert!(response["error"].is_null(), "{response}");
        for (id, action) in [(8, "plan"), (9, "restore")] {
            request_tx.send(serde_json::json!({
                "jsonrpc":"2.0", "id":id, "method":"session/workspace_checkpoint",
                "params":{"sessionId":collision, "action":action, "boundaryId":"reused"},
            })).unwrap();
            let guarded = recv_response_with_id(&mut response_rx, id).await;
            assert_eq!(guarded["result"]["available"], false, "{guarded}");
            assert_eq!(guarded["result"]["reason"], "capture_failed", "{guarded}");
        }
        let non_git = tempfile::TempDir::new().unwrap();
        request_tx.send(serde_json::json!({
            "jsonrpc":"2.0", "id":10, "method":"session/new",
            "params":{"cwd":non_git.path().to_string_lossy(), "environmentPolicy":{"kind":"isolated", "grants":[]}, "workspaceCheckpoint":true},
        })).unwrap();
        let created = recv_response_with_id(&mut response_rx, 10).await;
        let unavailable = created["result"]["sessionId"].as_str().unwrap().to_string();
        request_tx.send(serde_json::json!({
            "jsonrpc":"2.0", "id":11, "method":"session/prompt",
            "params":{"sessionId":unavailable, "messageId":"no-git", "prompt":[{"type":"text", "text":"hello"}]},
        })).unwrap();
        let response = harn_clock::test_support::within("non-git prompt response", async {
            loop {
                let message = recv_json(&mut response_rx).await;
                if message["method"] == "host/capabilities" {
                    reply_empty_result(&request_tx, &message);
                } else if message["id"] == 11 {
                    break message;
                }
            }
        }).await;
        assert!(response["error"].is_null(), "{response}");
        request_tx.send(serde_json::json!({
            "jsonrpc":"2.0", "id":12, "method":"session/workspace_checkpoint",
            "params":{"sessionId":unavailable, "action":"plan", "boundaryId":"no-git"},
        })).unwrap();
        let unavailable_plan = recv_response_with_id(&mut response_rx, 12).await;
        assert_eq!(unavailable_plan["result"]["reason"], "capture_failed", "{unavailable_plan}");
        assert!(unavailable_plan["result"]["detail"].as_str().unwrap_or_default().contains("not_a_git_worktree"), "{unavailable_plan}");
        harn_hostlib::workspace_checkpoint::drop_session(&session);
        harn_hostlib::workspace_checkpoint::drop_session(&child);
        harn_hostlib::workspace_checkpoint::drop_session(&collision);
    }).await;
}

#[cfg(feature = "hostlib")]
#[tokio::test(flavor = "current_thread")]
async fn historical_fork_restores_its_matching_file_boundary() {
    use harn_session_store::{AppendEvent, SessionEventKind, SessionStore};
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    let git = |args: &[&str]| {
        assert!(std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .status()
            .unwrap()
            .success());
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "t@example.com"]);
    git(&["config", "user.name", "t"]);
    std::fs::write(root.join("first.txt"), "first before").unwrap();
    std::fs::write(root.join("second.txt"), "second before").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "init"]);

    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut server = AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));
    server.handle_incoming_message(serde_json::json!({"jsonrpc":"2.0", "id":1, "method":"session/new", "params":{"cwd":root.to_string_lossy(), "environmentPolicy":{"kind":"isolated", "grants":[]}}})).await;
    let created = recv_response_with_id(&mut rx, 1).await;
    let parent = created["result"]["sessionId"].as_str().unwrap().to_string();
    let store = harn_vm::open_canonical_store(root).unwrap();
    let workspace = root.canonicalize().unwrap().display().to_string();
    let admitted = store.describe(&parent).await.unwrap();
    assert_eq!(admitted.cwd.as_deref(), Some(workspace.as_str()));
    assert_eq!(admitted.project_scope.as_deref(), Some(workspace.as_str()));
    for (identity, text) in [("turn-1", "first prompt"), ("turn-2", "second prompt")] {
        let mut event = AppendEvent::new(
            SessionEventKind::Message,
            serde_json::json!({"raw_message":{"role":"user", "content":text}}),
        );
        event
            .headers
            .insert("source_event_id".into(), identity.into());
        store.append(&parent, event).await.unwrap();
    }
    let history = harn_vm::agent_sessions::canonical_history_boundaries(&store, root, &parent)
        .await
        .unwrap();
    assert_eq!(history.positions.len(), 2);
    let before_second = history
        .positions
        .iter()
        .find(|p| p.source_event_id == "turn-2")
        .unwrap()
        .before_boundary
        .clone();
    for (capture_id, begin_id, end_id, boundary, call_id, path, content) in [
        (2, 3, 4, "turn-1", "edit-1", "first.txt", "first agent edit"),
        (
            5,
            6,
            7,
            "turn-2",
            "edit-2",
            "second.txt",
            "second agent edit",
        ),
    ] {
        for (id, action, key, value) in [
            (capture_id, "capture", "boundaryId", boundary),
            (begin_id, "call_begin", "callId", call_id),
        ] {
            let mut params = serde_json::json!({"sessionId":parent,"action":action});
            params[key] = serde_json::json!(value);
            server.handle_incoming_message(serde_json::json!({"jsonrpc":"2.0","id":id,"method":"session/workspace_checkpoint","params":params})).await;
            let response = recv_response_with_id(&mut rx, id).await;
            assert_eq!(response["result"]["available"], true, "{response}");
        }
        std::fs::write(root.join(path), content).unwrap();
        server.handle_incoming_message(serde_json::json!({"jsonrpc":"2.0","id":end_id,"method":"session/workspace_checkpoint","params":{"sessionId":parent,"action":"call_end","callId":call_id}})).await;
        let response = recv_response_with_id(&mut rx, end_id).await;
        assert_eq!(response["result"]["changes"][0]["path"], path, "{response}");
    }
    server.handle_incoming_message(serde_json::json!({"jsonrpc":"2.0","id":8,"method":"session/fork","params":{"sessionId":parent,"canonicalBoundary":before_second,"inheritWorkspaceCheckpoint":true}})).await;
    let fork = recv_response_with_id(&mut rx, 8).await;
    assert_eq!(
        fork["result"]["workspaceCheckpoint"]["available"], true,
        "{fork}"
    );
    let child = fork["result"]["sessionId"].as_str().unwrap().to_string();
    let transcript =
        harn_vm::llm::vm_value_to_json(&harn_vm::agent_sessions::transcript(&child).unwrap());
    assert_eq!(
        transcript["messages"].as_array().unwrap().len(),
        1,
        "{transcript}"
    );
    assert_eq!(transcript["messages"][0]["content"], "first prompt");
    server.handle_incoming_message(serde_json::json!({"jsonrpc":"2.0","id":9,"method":"session/workspace_checkpoint","params":{"sessionId":child,"action":"restore","boundaryId":"turn-2"}})).await;
    let restore = recv_response_with_id(&mut rx, 9).await;
    assert_eq!(
        restore["result"]["restored"][0]["path"], "second.txt",
        "{restore}"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("first.txt")).unwrap(),
        "first agent edit"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("second.txt")).unwrap(),
        "second before"
    );
    harn_hostlib::workspace_checkpoint::drop_session(&parent);
    harn_hostlib::workspace_checkpoint::drop_session(&child);
}

#[cfg(feature = "hostlib")]
async fn check_captured_in_workspace_state_root(root: &std::path::Path) {
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.email", "t@example.com"],
        vec!["config", "user.name", "t"],
    ] {
        assert!(std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .status()
            .unwrap()
            .success());
    }
    std::fs::write(root.join("agent.txt"), "before").unwrap();
    std::fs::create_dir(root.join("custom-runtime")).unwrap();
    std::fs::write(root.join("custom-runtime/.gitignore"), "").unwrap();
    std::fs::write(root.join("custom-runtime/user.txt"), "user before").unwrap();
    for args in [vec!["add", "-A"], vec!["commit", "-q", "-m", "init"]] {
        assert!(std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .status()
            .unwrap()
            .success());
    }
    let state_root = root.join("custom-runtime");
    let launcher =
        harn_vm::security::LauncherEnvironment::from_snapshot(std::collections::BTreeMap::from([
            (
                "HARN_STATE_DIR".into(),
                state_root
                    .join("../custom-runtime")
                    .to_string_lossy()
                    .into_owned(),
            ),
        ]));
    let config = AcpServerConfig::new(None).with_launcher_environment(launcher);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut server = AcpServer::new_with_output(config, AcpOutput::Channel(tx));
    server.handle_incoming_message(serde_json::json!({
        "jsonrpc":"2.0", "id":1, "method":"session/new",
        "params":{"cwd":root.to_string_lossy(), "environmentPolicy":{"kind":"isolated", "grants":[]}},
    })).await;
    let created = recv_response_with_id(&mut rx, 1).await;
    let session = created["result"]["sessionId"]
        .as_str()
        .expect("session id")
        .to_string();
    for (id, action, key, value) in [
        (2, "capture", "boundaryId", "turn-1"),
        (3, "call_begin", "callId", "edit-1"),
    ] {
        let mut params = serde_json::json!({"sessionId":session, "action":action});
        params[key] = serde_json::json!(value);
        server.handle_incoming_message(serde_json::json!({"jsonrpc":"2.0", "id":id, "method":"session/workspace_checkpoint", "params":params})).await;
        let response = recv_response_with_id(&mut rx, id).await;
        assert_eq!(response["result"]["available"], true, "{response}");
    }
    server.handle_incoming_message(serde_json::json!({"jsonrpc":"2.0", "id":4, "method":"session/fork", "params":{"sessionId":session}})).await;
    let fork = recv_response_with_id(&mut rx, 4).await;
    assert!(fork["result"]["sessionId"].is_string(), "{fork}");
    std::fs::write(root.join("agent.txt"), "agent edited").unwrap();
    std::fs::write(root.join("custom-runtime/user.txt"), "user edited").unwrap();
    server.handle_incoming_message(serde_json::json!({"jsonrpc":"2.0", "id":5, "method":"session/workspace_checkpoint", "params":{"sessionId":session, "action":"call_end", "callId":"edit-1"}})).await;
    let end = recv_response_with_id(&mut rx, 5).await;
    let changes = end["result"]["changes"]
        .as_array()
        .expect("attributed changes");
    let changed: Vec<&str> = changes
        .iter()
        .map(|change| change["path"].as_str().unwrap())
        .collect();
    assert_eq!(
        changed,
        ["agent.txt", "custom-runtime/user.txt"],
        "{changes:?}"
    );
    server.handle_incoming_message(serde_json::json!({"jsonrpc":"2.0", "id":6, "method":"session/workspace_checkpoint", "params":{"sessionId":session, "action":"restore", "boundaryId":"turn-1"}})).await;
    let restored = recv_response_with_id(&mut rx, 6).await;
    assert_eq!(
        restored["result"]["restored"].as_array().unwrap().len(),
        2,
        "{restored}"
    );
    assert_eq!(
        restored["result"]["restored"][0]["path"], "agent.txt",
        "{restored}"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("agent.txt")).unwrap(),
        "before"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("custom-runtime/user.txt")).unwrap(),
        "user before"
    );
    assert!(state_root.join("session-store.sqlite").is_file());
    assert!(state_root.join("agent-run-writers/project.lock").is_file());
    harn_hostlib::workspace_checkpoint::drop_session(&session);
}

#[cfg(feature = "hostlib")]
#[tokio::test(flavor = "current_thread")]
async fn acp_checkpoint_preserves_a_captured_in_workspace_state_root() {
    let dir = tempfile::TempDir::new().unwrap();
    check_captured_in_workspace_state_root(dir.path()).await;
}

#[cfg(all(feature = "hostlib", unix))]
#[tokio::test(flavor = "current_thread")]
async fn acp_checkpoint_excludes_custom_runtime_through_workspace_alias() {
    let dir = tempfile::TempDir::new().unwrap();
    let physical = dir.path().join("workspace");
    let alias = dir.path().join("workspace-alias");
    std::fs::create_dir(&physical).unwrap();
    std::os::unix::fs::symlink(&physical, &alias).unwrap();
    check_captured_in_workspace_state_root(&alias).await;
}

#[cfg(feature = "hostlib")]
#[tokio::test(flavor = "current_thread")]
async fn acp_workspace_checkpoint_restores_attributed_edit_and_refuses_diverged_file() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    let git = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?}");
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "t@example.com"]);
    git(&["config", "user.name", "t"]);
    std::fs::write(root.join("agent.txt"), "agent original").unwrap();
    std::fs::write(root.join("contested.txt"), "contested original").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "init"]);

    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut server = AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));
    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "session/new",
            "params": {"cwd": root.to_string_lossy(), "environmentPolicy": {"kind": "isolated", "grants": []}},
        }))
        .await;
    let session_id = recv_json(&mut rx).await["result"]["sessionId"]
        .as_str()
        .expect("session id")
        .to_string();

    let mut next_id = 2;
    let mut call = |action: &str, extra: serde_json::Value| {
        let mut params = serde_json::json!({"sessionId": session_id, "action": action});
        params
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        next_id += 1;
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": next_id,
            "method": "session/workspace_checkpoint",
            "params": params,
        })
    };

    server
        .handle_incoming_message(call("capture", serde_json::json!({"boundaryId": "turn-1"})))
        .await;
    assert_eq!(recv_json(&mut rx).await["result"]["available"], true);

    server
        .handle_incoming_message(call("call_begin", serde_json::json!({"callId": "edit-1"})))
        .await;
    assert_eq!(recv_json(&mut rx).await["result"]["available"], true);
    // Forking opens the live session store while the workspace call is active.
    // Its database and lock files must not become edits eligible for restore.
    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0", "id": 100, "method": "session/fork",
            "params": {"sessionId": session_id},
        }))
        .await;
    let fork = recv_response_with_id(&mut rx, 100).await;
    assert!(fork["result"]["sessionId"].is_string(), "{fork}");
    std::fs::write(root.join("agent.txt"), "agent edited, longer").unwrap();
    std::fs::write(root.join("contested.txt"), "contested by the agent").unwrap();
    server
        .handle_incoming_message(call(
            "call_end",
            serde_json::json!({"callId": "edit-1", "sideEffect": "workspace_write"}),
        ))
        .await;
    let attributed = recv_json(&mut rx).await;
    assert_eq!(attributed["result"]["changes"].as_array().unwrap().len(), 2);

    std::fs::write(root.join("contested.txt"), "contested by a person").unwrap();
    server
        .handle_incoming_message(call("restore", serde_json::json!({"boundaryId": "turn-1"})))
        .await;
    let receipt = recv_json(&mut rx).await;
    assert_eq!(receipt["result"]["restored"][0]["path"], "agent.txt");
    assert_eq!(receipt["result"]["refused"][0]["path"], "contested.txt");
    assert_eq!(
        std::fs::read_to_string(root.join("agent.txt")).unwrap(),
        "agent original"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("contested.txt")).unwrap(),
        "contested by a person"
    );
    assert!(root.join(".harn/session-store.sqlite").is_file());
    assert!(root.join(".harn/agent-run-writers/project.lock").is_file());

    // A malformed path filter is rejected, never widened to "restore everything".
    std::fs::write(root.join("agent.txt"), "edited again").unwrap();
    for bad in [
        serde_json::json!("agent.txt"),
        serde_json::json!([1]),
        serde_json::json!([]),
    ] {
        server
            .handle_incoming_message(call(
                "restore",
                serde_json::json!({"boundaryId": "turn-1", "paths": bad}),
            ))
            .await;
        assert_eq!(recv_json(&mut rx).await["error"]["code"], -32602);
    }
    assert_eq!(
        std::fs::read_to_string(root.join("agent.txt")).unwrap(),
        "edited again"
    );

    server
        .handle_incoming_message(call("bogus", serde_json::json!({})))
        .await;
    assert_eq!(recv_json(&mut rx).await["error"]["code"], -32602);
    harn_hostlib::workspace_checkpoint::drop_session(&session_id);
}

#[cfg(feature = "hostlib")]
#[tokio::test(flavor = "current_thread")]
async fn acp_fork_inherits_independent_workspace_checkpoints() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    let git = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?}");
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "t@example.com"]);
    git(&["config", "user.name", "t"]);
    std::fs::write(root.join("a.txt"), "committed").unwrap();
    std::fs::write(root.join("b.txt"), "committed").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "init"]);
    std::fs::write(root.join("a.txt"), "dirty baseline a").unwrap();
    std::fs::write(root.join("b.txt"), "dirty baseline b").unwrap();

    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut server = AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));
    server.handle_incoming_message(serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "session/new",
        "params": {"cwd": root.to_string_lossy(), "environmentPolicy": {"kind": "isolated", "grants": []}},
    })).await;
    let parent = recv_response_with_id(&mut rx, 1).await["result"]["sessionId"]
        .as_str()
        .unwrap()
        .to_string();

    for (id, action, extra) in [
        (2, "capture", serde_json::json!({"boundaryId": "turn-1"})),
        (3, "call_begin", serde_json::json!({"callId": "edit-1"})),
    ] {
        let mut params = serde_json::json!({"sessionId": parent, "action": action});
        params
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        server.handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0", "id": id, "method": "session/workspace_checkpoint", "params": params,
        })).await;
        assert_eq!(
            recv_response_with_id(&mut rx, id).await["result"]["available"],
            true
        );
    }
    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0", "id": 10, "method": "session/fork",
            "params": {"sessionId": parent, "inheritWorkspaceCheckpoint": true},
        }))
        .await;
    let pending_fork = recv_response_with_id(&mut rx, 10).await;
    assert_eq!(
        pending_fork["result"]["workspaceCheckpoint"]["available"],
        false
    );
    assert_eq!(
        pending_fork["result"]["workspaceCheckpoint"]["reason"],
        "calls_in_flight"
    );
    for (id, action) in [(13, "plan"), (14, "restore")] {
        server
            .handle_incoming_message(serde_json::json!({
                "jsonrpc": "2.0", "id": id, "method": "session/workspace_checkpoint",
                "params": {"sessionId": parent, "action": action, "boundaryId": "turn-1"},
            }))
            .await;
        let pending = recv_response_with_id(&mut rx, id).await;
        assert_eq!(pending["result"]["available"], false, "{pending}");
        assert_eq!(pending["result"]["reason"], "calls_in_flight");
        assert!(pending["result"]["detail"]
            .as_str()
            .unwrap()
            .contains("edit-1"));
    }
    std::fs::write(root.join("a.txt"), "agent changed a").unwrap();
    std::fs::write(root.join("b.txt"), "agent changed b").unwrap();
    server.handle_incoming_message(serde_json::json!({
        "jsonrpc": "2.0", "id": 4, "method": "session/workspace_checkpoint",
        "params": {"sessionId": parent, "action": "call_end", "callId": "edit-1", "sideEffect": "workspace_write"},
    })).await;
    let attributed = recv_response_with_id(&mut rx, 4).await;
    let changes = attributed["result"]["changes"].as_array().unwrap();
    assert_eq!(
        changes.len(),
        2,
        "unexpected attributed changes: {changes:?}"
    );

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0", "id": 8, "method": "session/fork",
            "params": {"sessionId": parent},
        }))
        .await;
    let ordinary_fork = recv_response_with_id(&mut rx, 8).await;
    assert!(ordinary_fork["result"].get("workspaceCheckpoint").is_none());
    let ordinary_child = ordinary_fork["result"]["sessionId"]
        .as_str()
        .unwrap()
        .to_string();
    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0", "id": 9, "method": "session/workspace_checkpoint",
            "params": {"sessionId": ordinary_child, "action": "plan", "boundaryId": "turn-1"},
        }))
        .await;
    assert_eq!(
        recv_response_with_id(&mut rx, 9).await["result"]["available"],
        false
    );

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0", "id": 5, "method": "session/fork",
            "params": {"sessionId": parent, "inheritWorkspaceCheckpoint": true},
        }))
        .await;
    let fork = recv_response_with_id(&mut rx, 5).await;
    assert_eq!(
        fork["result"]["workspaceCheckpoint"]["available"], true,
        "{fork}"
    );
    let child = fork["result"]["sessionId"].as_str().unwrap().to_string();
    std::fs::write(root.join("b.txt"), "human changed b").unwrap();
    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0", "id": 6, "method": "session/workspace_checkpoint",
            "params": {"sessionId": child, "action": "restore", "boundaryId": "turn-1"},
        }))
        .await;
    let receipt = recv_response_with_id(&mut rx, 6).await;
    assert_eq!(receipt["result"]["restored"][0]["path"], "a.txt");
    assert_eq!(receipt["result"]["refused"][0]["path"], "b.txt");
    assert_eq!(
        std::fs::read_to_string(root.join("a.txt")).unwrap(),
        "dirty baseline a"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("b.txt")).unwrap(),
        "human changed b"
    );

    let parent_plan = harn_hostlib::workspace_checkpoint::plan(&parent, "turn-1").unwrap();
    let parent_plan = serde_json::to_value(parent_plan).unwrap();
    assert_eq!(parent_plan["entries"].as_array().unwrap().len(), 2);
    let other_root = tempfile::TempDir::new().unwrap();
    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0", "id": 11, "method": "harn.session_reanchor",
            "params": {"sessionId": parent, "path": other_root.path().to_string_lossy()},
        }))
        .await;
    let reanchor = recv_response_with_id(&mut rx, 11).await;
    assert_eq!(reanchor["result"]["changed"], true, "{reanchor}");
    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0", "id": 12, "method": "session/workspace_checkpoint",
            "params": {"sessionId": parent, "action": "restore", "boundaryId": "turn-1"},
        }))
        .await;
    let wrong_root = recv_response_with_id(&mut rx, 12).await;
    assert_eq!(wrong_root["result"]["available"], false, "{wrong_root}");
    assert_eq!(wrong_root["result"]["reason"], "workspace_root_changed");
    assert_eq!(
        std::fs::read_to_string(root.join("b.txt")).unwrap(),
        "human changed b"
    );
    assert!(harn_hostlib::workspace_checkpoint::drop_session(&parent).dropped);
    std::fs::write(root.join("b.txt"), "agent changed b").unwrap();
    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0", "id": 7, "method": "session/workspace_checkpoint",
            "params": {"sessionId": child, "action": "restore", "boundaryId": "turn-1"},
        }))
        .await;
    let retry = recv_response_with_id(&mut rx, 7).await;
    assert_eq!(retry["result"]["restored"][0]["path"], "b.txt", "{retry}");
    assert_eq!(
        std::fs::read_to_string(root.join("b.txt")).unwrap(),
        "dirty baseline b"
    );
    harn_hostlib::workspace_checkpoint::drop_session(&child);
}

/// A host that opts in gets per-call attribution from the agent loop itself:
/// it never brackets tool calls, yet the plan names the file a tool wrote.
#[cfg(feature = "hostlib")]
#[tokio::test(flavor = "current_thread")]
async fn agent_loop_tool_calls_are_attributed_without_host_bracketing() {
    harn_clock::test_support::with_local_tasks(async {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let git = |args: &[&str]| {
            assert!(std::process::Command::new("git").arg("-C").arg(&root).args(args).status().unwrap().success());
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@example.com"]);
        git(&["config", "user.name", "t"]);
        std::fs::write(root.join("agent.txt"), "agent original").unwrap();
        std::fs::write(root.join("seen.txt"), "read only").unwrap();
        let target = harn_lexer::escape_string_literal(&root.join("agent.txt").to_string_lossy());
        let pipeline = root.join("test.harn");
        std::fs::write(&pipeline, format!(r#"import {{ agent_loop }} from "std/agent/loop"

pipeline default(harness: Harness, task: unknown) {{
  harness.llm.mock_clear()
  harness.llm.mock_enqueue({{text: "", tool_calls: [{{id: "edit", name: "edit", arguments: {{}}}}]}})
  harness.llm.mock_enqueue({{text: "", tool_calls: [{{id: "look", name: "look", arguments: {{}}}}]}})
  harness.llm.mock_enqueue({{text: "done"}})
  let tools = tool_registry()
  tools = tool_define(tools, "look", "Look", {{parameters: {{}},
    annotations: {{kind: "read", side_effect_level: "read_only"}},
    handler: {{ _args -> "seen" }},
  }})
  tools = tool_define(tools, "edit", "Edit", {{parameters: {{}},
    annotations: {{kind: "edit", side_effect_level: "workspace_write"}},
    handler: {{ _args ->
      harness.fs.write_text("{target}", "agent edited")
      return "edited"
    }},
  }})
  agent_loop(harness, "Edit", nil, {{provider: "mock", tools: tools,
    tool_format: "native", max_iterations: 3}})
}}
"#)).unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "init"]);

        let (request_tx, request_rx) = mpsc::unbounded_channel();
        let (response_tx, mut response_rx) = mpsc::unbounded_channel();
        let config = AcpServerConfig::for_pipeline(pipeline.to_string_lossy().into_owned());
        let _server = tokio::task::spawn_local(super::super::run_acp_channel_server(config, request_rx, response_tx));
        request_tx.send(serde_json::json!({
            "jsonrpc":"2.0", "id":1, "method":"session/new",
            "params":{"cwd":root.to_string_lossy(), "environmentPolicy":{"kind":"isolated", "grants":[]}, "workspaceCheckpoint":true},
        })).unwrap();
        let created = recv_response_with_id(&mut response_rx, 1).await;
        let session = created["result"]["sessionId"].as_str().expect("session id").to_string();
        request_tx.send(serde_json::json!({
            "jsonrpc":"2.0", "id":3, "method":"session/set_mode",
            "params":{"sessionId":session, "modeId":"code"},
        })).unwrap();
        recv_response_with_id(&mut response_rx, 3).await;
        request_tx.send(serde_json::json!({
            "jsonrpc":"2.0", "id":2, "method":"session/prompt",
            "params":{"sessionId":session, "messageId":"turn-1", "prompt":[{"type":"text", "text":"edit"}]},
        })).unwrap();
        let response = harn_clock::test_support::within("tool turn response", async {
            loop {
                let message = recv_json(&mut response_rx).await;
                if message["method"] == "host/capabilities" {
                    reply_empty_result(&request_tx, &message);
                } else if message["id"] == 2 {
                    break message;
                }
            }
        }).await;
        assert!(response["error"].is_null(), "{response}");
        assert_eq!(std::fs::read_to_string(root.join("agent.txt")).unwrap(), "agent edited", "{response}");

        let plan = serde_json::to_value(
            harn_hostlib::workspace_checkpoint::plan(&session, "turn-1").unwrap(),
        ).unwrap();
        let planned: Vec<&str> = plan["entries"].as_array().unwrap_or_else(|| panic!("{plan}"))
            .iter().filter_map(|entry| entry["path"].as_str()).collect();
        assert_eq!(planned, vec!["agent.txt"], "{plan}");

        let receipt = serde_json::to_value(
            harn_hostlib::workspace_checkpoint::restore(&session, "turn-1", None).unwrap(),
        ).unwrap();
        assert_eq!(receipt["restored"][0]["path"], "agent.txt", "{receipt}");
        assert_eq!(std::fs::read_to_string(root.join("agent.txt")).unwrap(), "agent original");
        harn_hostlib::workspace_checkpoint::drop_session(&session);
    }).await;
}
