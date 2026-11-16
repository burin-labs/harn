use super::*;

fn tracked_dirty_workspace() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let global_config = root.join("git-global-config");
    std::fs::write(&global_config, "").unwrap();
    std::fs::write(root.join("source.rs"), "initial\n").unwrap();
    for args in [vec!["init", "-q"], vec!["add", "source.rs"]] {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .env("GIT_CONFIG_GLOBAL", &global_config)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_COMMON_DIR")
            .env_remove("GIT_INDEX_FILE")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    std::fs::write(root.join("source.rs"), "initial\nuser change\n").unwrap();
    directory
}

#[tokio::test]
async fn tracked_dirty_writes_follow_ordinary_policy_and_keep_write_intent() {
    clear_command_policies();
    let workspace = tracked_dirty_workspace();
    let root = workspace.path().to_str().unwrap();
    for command in [
        "printf updated > source.rs",
        "printf appended >> source.rs",
        "truncate -s 0 source.rs",
        "printf '%s' \"$(: > source.rs)\"",
        "printf updated > 123",
        "printf updated > -",
    ] {
        let mut context = shell_ctx(command);
        context["request"]["cwd"] = serde_json::json!(root);
        context["workspace_roots"] = serde_json::json!([root]);
        for context in [context, cat_ctx_argv(&["sh", "-c", command], &[root])] {
            let scan = command_risk_scan_json(&context, None);
            assert!(
                labels(&scan).contains(&"write_intent".to_string()),
                "{command}: {scan}"
            );
            assert!(
                scan.get("catastrophic_reason").is_none(),
                "{command}: {scan}"
            );
        }
        let mut params = shell_params(command);
        params.put_str("cwd", root);
        push_command_policy(CommandPolicy {
            workspace_roots: vec![root.to_string()],
            ..Default::default()
        });
        assert_proceed(
            &run_command_policy_preflight(&params, JsonValue::Null)
                .await
                .unwrap(),
        );
        clear_command_policies();

        push_command_policy(CommandPolicy {
            workspace_roots: vec![root.to_string()],
            deny_labels: BTreeSet::from(["write_intent".to_string()]),
            ..Default::default()
        });
        let refused = run_command_policy_preflight(&params, JsonValue::Null)
            .await
            .unwrap();
        match refused {
            CommandPolicyPreflight::Blocked {
                status, decisions, ..
            } => {
                assert_eq!(status, "blocked");
                assert!(decisions
                    .iter()
                    .any(|decision| decision.source == "deny_labels"
                        && decision.risk_labels.contains(&"write_intent".to_string())));
                assert!(!decisions
                    .iter()
                    .any(|decision| decision.source == "catastrophic_floor"));
            }
            other => panic!("write-restricted policy must refuse {command}: {other:?}"),
        }
        clear_command_policies();
    }
    for command in [
        "printf '%s' ': > source.rs'",
        "printf x > /dev/null",
        "printf x 2>&1",
    ] {
        let scan = command_risk_scan_json(&shell_ctx(command), None);
        assert!(
            !labels(&scan).contains(&"write_intent".to_string()),
            "{command}: {scan}"
        );
    }
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("source.rs")).unwrap(),
        "initial\nuser change\n"
    );
}

#[tokio::test]
async fn tracked_writes_remain_approval_eligible_and_destructive_commands_do_not() {
    clear_command_policies();
    let workspace = tracked_dirty_workspace();
    let root = workspace.path().to_str().unwrap();
    push_command_policy(CommandPolicy {
        workspace_roots: vec![root.to_string()],
        require_approval: BTreeSet::from(["write_intent".to_string(), "catastrophic".to_string()]),
        ..Default::default()
    });
    let mut params = shell_params("printf updated > source.rs");
    params.put_str("cwd", root);
    match run_command_policy_preflight(&params, JsonValue::Null)
        .await
        .unwrap()
    {
        CommandPolicyPreflight::Blocked { decisions, .. } => {
            assert!(decisions
                .iter()
                .any(|decision| decision.action == "require_approval"
                    && decision.risk_labels.contains(&"write_intent".to_string())));
            assert!(!decisions
                .iter()
                .any(|decision| decision.source == "catastrophic_floor"));
        }
        other => panic!("missing approval must not silently allow a write: {other:?}"),
    }
    for command in [
        "git reset --hard",
        "rm -rf /",
        "mkfs.ext4 /dev/fake-owned-fixture",
    ] {
        params.put_str("command", command);
        assert_floor_blocked(
            &run_command_policy_preflight(&params, JsonValue::Null)
                .await
                .unwrap(),
        );
    }
    clear_command_policies();
}
