use super::*;

#[tokio::test(flavor = "current_thread")]
async fn correlates_delegated_runs() {
    let _guard = lock_harn_state_async().await;
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);
    let parent_path = temp.path().join("parent.json");
    let child_path = temp.path().join("child.json");
    let child = harn_vm::orchestration::RunRecord {
        type_name: "workflow_run".to_string(),
        id: "child".to_string(),
        workflow_id: "child-workflow".to_string(),
        status: "completed".to_string(),
        parent_run_id: Some("parent".to_string()),
        root_run_id: Some("parent".to_string()),
        ..harn_vm::orchestration::RunRecord::default()
    };
    let parent = harn_vm::orchestration::RunRecord {
        type_name: "workflow_run".to_string(),
        id: "parent".to_string(),
        workflow_id: "parent-workflow".to_string(),
        status: "completed".to_string(),
        root_run_id: Some("parent".to_string()),
        child_runs: vec![harn_vm::orchestration::RunChildRecord {
            worker_id: "worker-1".to_string(),
            worker_name: "child".to_string(),
            status: "completed".to_string(),
            run_id: Some("child".to_string()),
            run_path: Some(child_path.to_string_lossy().into_owned()),
            ..harn_vm::orchestration::RunChildRecord::default()
        }],
        ..harn_vm::orchestration::RunRecord::default()
    };
    harn_vm::orchestration::save_run_record(&child, Some(child_path.to_str().unwrap())).unwrap();
    harn_vm::orchestration::save_run_record(&parent, Some(parent_path.to_str().unwrap())).unwrap();
    let service = McpOrchestratorService::new(&fixture_args(&temp)).unwrap();
    let mut session = init_session(&service).await;

    let report = call_tool(
        &service,
        &mut session,
        "harn.run.report",
        json!({ "path": "parent.json" }),
    )
    .await;
    assert_eq!(report["schema"], "harn.run_report.v1");
    assert_eq!(report["root_run_id"], "parent");
    assert_eq!(report["agents"].as_array().unwrap().len(), 2);
    assert_eq!(report["delegations"][0]["forward_pointer"], true);
    assert_eq!(report["delegations"][0]["back_pointer"], true);
    assert!(report["projection"]["hash"]
        .as_str()
        .is_some_and(|hash| hash.starts_with("sha256:")));
}

#[tokio::test(flavor = "current_thread")]
async fn rejects_project_root_escape() {
    let _guard = lock_harn_state_async().await;
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);
    let outside = TempDir::new().unwrap();
    let outside_path = outside.path().join("run.json");
    let run = harn_vm::orchestration::RunRecord {
        type_name: "workflow_run".to_string(),
        id: "outside".to_string(),
        workflow_id: "outside-workflow".to_string(),
        ..harn_vm::orchestration::RunRecord::default()
    };
    harn_vm::orchestration::save_run_record(&run, Some(outside_path.to_str().unwrap())).unwrap();
    let service = McpOrchestratorService::new(&fixture_args(&temp)).unwrap();
    let mut session = init_session(&service).await;
    let response = service
        .handle_request(
            &mut session,
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "harn.run.report",
                    "arguments": { "path": outside_path },
                }
            }),
        )
        .await;
    assert_eq!(response["result"]["isError"], true, "response={response}");
    assert!(response["result"]["content"][0]["text"]
        .as_str()
        .is_some_and(|text| text.contains("outside the report's allowed roots")));
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn rejects_symlink_escape() {
    let _guard = lock_harn_state_async().await;
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);
    let outside = TempDir::new().unwrap();
    let outside_path = outside.path().join("run.json");
    let link_path = temp.path().join("linked-run.json");
    let run = harn_vm::orchestration::RunRecord {
        type_name: "workflow_run".to_string(),
        id: "outside".to_string(),
        workflow_id: "outside-workflow".to_string(),
        ..harn_vm::orchestration::RunRecord::default()
    };
    harn_vm::orchestration::save_run_record(&run, Some(outside_path.to_str().unwrap())).unwrap();
    std::os::unix::fs::symlink(&outside_path, &link_path).unwrap();
    let service = McpOrchestratorService::new(&fixture_args(&temp)).unwrap();
    let mut session = init_session(&service).await;
    let response = service
        .handle_request(
            &mut session,
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "harn.run.report",
                    "arguments": { "path": "linked-run.json" },
                }
            }),
        )
        .await;

    assert_eq!(response["result"]["isError"], true, "response={response}");
    assert!(response["result"]["content"][0]["text"]
        .as_str()
        .is_some_and(|text| text.contains("outside the report's allowed roots")));
}
