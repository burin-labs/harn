use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{json, Value as JsonValue};

use harn_vm::{
    compile_source, install_active_connector_clients, register_vm_stdlib, reset_thread_local_state,
    ClientError, ConnectorClient, ProviderId, Vm, VmError,
};

#[derive(Default)]
struct RecordingClient {
    calls: Mutex<Vec<(String, JsonValue)>>,
}

#[async_trait]
impl ConnectorClient for RecordingClient {
    async fn call(&self, method: &str, args: JsonValue) -> Result<JsonValue, ClientError> {
        self.calls
            .lock()
            .expect("recording client calls lock")
            .push((method.to_string(), args));
        Ok(match method {
            "github.actions.workflow_dispatch" => json!({"workflow_run_id": 123}),
            "github.actions.runs" => json!({"workflow_runs": [{"id": 123}]}),
            "github.actions.run" => json!({"id": 123, "status": "completed"}),
            "repos.get_text" => json!({"text": "v0.8.4\n"}),
            "github.release.latest" => json!({
                "ok": true,
                "tag_name": "v0.8.4",
                "asset_names": ["harn-aarch64-apple-darwin.tar.gz"]
            }),
            "github.release.assets" => json!({
                "ok": true,
                "release_id": 501,
                "asset_names": ["harn-x86_64-unknown-linux-gnu.tar.gz"]
            }),
            "github.pr.enable_auto_merge" => json!({
                "state": "already_enabled",
                "auto_merge": {"merge_method": "SQUASH"},
                "pull_request": {"html_url": "https://github.com/octo-org/octo-repo/pull/7"}
            }),
            "issues.create_comment" => json!({"id": 9001}),
            "issues.update" => json!({"number": 7, "state": "closed"}),
            other => return Err(ClientError::MethodNotFound(other.to_string())),
        })
    }
}

fn run_with_github_client(source: &str, client: Arc<RecordingClient>) -> Result<String, String> {
    reset_thread_local_state();
    let chunk = compile_source(source)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut clients: BTreeMap<ProviderId, Arc<dyn ConnectorClient>> = BTreeMap::new();
                clients.insert(ProviderId::from("github"), client);
                install_active_connector_clients(clients);

                let mut vm = Vm::new();
                register_vm_stdlib(&mut vm);
                vm.execute(&chunk)
                    .await
                    .map_err(|error: VmError| format!("{error:?}"))?;
                Ok(vm.output().to_string())
            })
            .await
    })
}

#[test]
fn github_stdlib_wrappers_dispatch_typed_connector_methods() {
    let client = Arc::new(RecordingClient::default());
    let output = run_with_github_client(
        r#"
import {
  close_pr,
  enable_auto_merge,
  latest_release,
  read_file_at_ref,
  release_assets,
  workflow_dispatch,
  workflow_run,
  workflow_runs,
} from "std/connectors/github"

pipeline main(task) {
  let dispatch = workflow_dispatch("git@github.com:octo-org/octo-repo.git", "release.yml", "main", {version: "v0.8.4"})
  assert_eq(dispatch.workflow_run_id, 123, "dispatch result")
  assert_eq(workflow_runs("octo-org/octo-repo", {event: "workflow_dispatch"}).workflow_runs[0].id, 123, "runs result")
  assert_eq(workflow_run("octo-org/octo-repo", 123).status, "completed", "run result")
  assert_eq(trim(read_file_at_ref("https://github.com/octo-org/octo-repo.git", ".harn-version", "main").text), "v0.8.4", "file text")
  assert_eq(latest_release("octo-org/octo-repo").tag_name, "v0.8.4", "release tag")
  assert_eq(release_assets("octo-org/octo-repo", 501).asset_names[0], "harn-x86_64-unknown-linux-gnu.tar.gz", "asset names")
  let auto_merge = enable_auto_merge("octo-org/octo-repo", 7, {method: "squash"})
  assert_eq(auto_merge.ok, true, "auto-merge ok")
  assert_eq(auto_merge.state, "already_enabled", "auto-merge state")
  assert_eq(auto_merge.strategy, "graphql_auto_merge", "auto-merge strategy")
  let closed = close_pr("octo-org/octo-repo", 7, "Closing in favor of the release branch.")
  assert_eq(closed.ok, true, "close ok")
  assert_eq(closed.comment_posted, true, "close comment")
  log("github wrappers ok")
}
"#,
        client.clone(),
    )
    .expect("Harn source should execute");

    assert!(output.contains("[harn] github wrappers ok"));
    let calls = client.calls.lock().expect("recording client calls lock");
    let methods = calls
        .iter()
        .map(|(method, _)| method.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        methods,
        vec![
            "github.actions.workflow_dispatch",
            "github.actions.runs",
            "github.actions.run",
            "repos.get_text",
            "github.release.latest",
            "github.release.assets",
            "github.pr.enable_auto_merge",
            "issues.create_comment",
            "issues.update",
        ]
    );
    assert_eq!(calls[0].1["repo"], "octo-org/octo-repo");
    assert_eq!(calls[0].1["workflow_id"], "release.yml");
    assert_eq!(calls[3].1["ref"], "main");
    assert_eq!(calls[6].1["pull_number"], 7);
    assert_eq!(calls[8].1["state"], "closed");
}
