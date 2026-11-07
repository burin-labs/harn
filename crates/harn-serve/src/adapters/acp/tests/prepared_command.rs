use super::*;
use tokio::io::{AsyncBufReadExt, BufReader};

#[tokio::test(flavor = "current_thread")]
async fn executor_bound_commands_reach_the_real_remote_child_and_refuse_unbound_hosts() {
    for mode in ["normal", "rewrite", "deny", "unsupported"] {
        remote_command(mode).await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn cancelling_an_executor_bound_command_releases_and_reaps_the_real_child() {
    remote_command("cancel").await;
}

async fn remote_command(mode: &str) {
    harn_clock::test_support::with_local_tasks(async {
        harn_vm::reset_thread_local_state();
        let vm_root = tempfile::tempdir().unwrap();
        let executor_root = tempfile::tempdir().unwrap();
        let executor_root = harn_vm::stdlib::process::child_process_cwd(executor_root.path().canonicalize().unwrap());
        let source = vm_root.path().join("prepared.harn");
        std::fs::write(&source, include_str!("fixtures/prepared_command.harn")).unwrap();
        let config = AcpServerConfig::for_pipeline(source.to_string_lossy().into_owned());
        let (tx, mut rx, server, session) = start_acp_channel_session_with_config(
            config, serde_json::json!(vm_root.path()),
        ).await;
        let python = if cfg!(windows) { "python" } else { "python3" };
        tx.send(serde_json::json!({
            "jsonrpc":"2.0", "id":3, "method":"session/prompt", "params":{
                "sessionId":session, "prompt":[{"type":"text", "text":serde_json::json!({
                    "root":executor_root, "mode":mode, "python":python,
                }).to_string()}],
            },
        })).unwrap();

        let mut prepared = BTreeMap::<String, tokio::process::Command>::new();
        let mut running = BTreeMap::<String, tokio::process::Child>::new();
        let mut prepare_count = 0;
        let mut execute_count = 0;
        let mut release_count = 0;
        let mut consent_count = 0;
        let mut completed_marker = false;
        let mut prompt_response = None;
        let mut cancelled_child = false;
        harn_clock::test_support::within("prepared remote command lifecycle", async {
            loop {
                let frame = recv_json(&mut rx).await;
                if let Some(text) = frame["params"]["update"]["content"]["text"].as_str() {
                    consent_count += text.matches("EXECUTOR_CONSENT_REACHED").count();
                    completed_marker |= text.contains("BOUND_COMMAND_DONE");
                }
                if frame["id"] == 3 && frame.get("method").is_none() {
                    if mode == "cancel" {
                        assert!(cancelled_child, "terminal cancellation must follow executor cleanup acknowledgement");
                    }
                    prompt_response = Some(frame.clone());
                }
                if let Some(method) = frame["method"].as_str().filter(|_| frame.get("id").is_some()) {
                    let result = match method {
                        "host/capabilities" => {
                            let ops = if mode == "unsupported" {
                                vec!["exec"]
                            } else {
                                vec!["exec", "prepare", "execute_prepared", "release_prepared"]
                            };
                            serde_json::json!({"process":{"ops":ops}})
                        }
                        "host/call" => {
                            let params = &frame["params"];
                            assert_eq!(params["sessionId"], session);
                            match params["name"].as_str().unwrap() {
                                "process.prepare" => {
                                    prepare_count += 1;
                                    assert_eq!(params["args"]["operation"], "exec");
                                    let request = &params["args"]["request"];
                                    let argv = request["argv"].as_array().unwrap().iter()
                                        .map(|arg| arg.as_str().unwrap().to_string()).collect::<Vec<_>>();
                                    let mut command = tokio::process::Command::new(&argv[0]);
                                    command.args(&argv[1..]).current_dir(&executor_root)
                                        .stdin(std::process::Stdio::null())
                                        .stdout(std::process::Stdio::piped())
                                        .stderr(std::process::Stdio::piped()).kill_on_drop(true);
                                    let token = format!("request-{prepare_count}");
                                    assert!(prepared.insert(token.clone(), command).is_none());
                                    serde_json::json!({"status":"bound", "token":token,
                                        "invocation":{"argv":argv, "cwd":executor_root}})
                                }
                                "process.execute_prepared" => {
                                    execute_count += 1;
                                    assert!(consent_count > 0, "actual consent must precede execution");
                                    let args = params["args"].as_object().unwrap();
                                    assert_eq!(args.len(), 1, "execution cannot replace retained launch facts");
                                    let token = args["token"].as_str().unwrap();
                                    let mut command = prepared.remove(token).expect("single-use prepared command");
                                    if mode == "cancel" {
                                        let mut child = command.spawn().unwrap();
                                        let stdout = child.stdout.take().unwrap();
                                        let mut lines = BufReader::new(stdout).lines();
                                        assert_eq!(lines.next_line().await.unwrap().as_deref(), Some("READY"));
                                        running.insert(token.to_string(), child);
                                        tx.send(serde_json::json!({"jsonrpc":"2.0", "method":"session/cancel",
                                            "params":{"sessionId":session}})).unwrap();
                                        continue;
                                    }
                                    let output = command.output().await.unwrap();
                                    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
                                    serde_json::json!({"status":"completed", "exit_code":output.status.code(),
                                        "stdout":String::from_utf8(output.stdout).unwrap(),
                                        "stderr":String::from_utf8(output.stderr).unwrap(), "success":true, "timed_out":false})
                                }
                                "process.release_prepared" => {
                                    release_count += 1;
                                    let token = params["args"]["token"].as_str().unwrap();
                                    prepared.remove(token);
                                    if let Some(mut child) = running.remove(token) {
                                        child.kill().await.unwrap();
                                        assert!(!child.wait().await.unwrap().success());
                                        cancelled_child = true;
                                    }
                                    serde_json::json!({"status":"released", "token":token})
                                }
                                "project.mcp_config" => serde_json::json!([]),
                                other => panic!("unexpected executor operation: {other}"),
                            }
                        }
                        other => panic!("unexpected client request: {other}"),
                    };
                    tx.send(serde_json::json!({"jsonrpc":"2.0", "id":frame["id"], "result":result})).unwrap();
                }
                if prompt_response.is_some() && release_count == prepare_count {
                    break;
                }
            }
        }).await;
        let response = prompt_response.unwrap();
        if mode == "cancel" {
            assert!(cancelled_child, "cancellation must reach and reap the executor's real child");
            assert_eq!(execute_count, 1);
        } else {
            assert!(response.get("error").is_none(), "{mode}: {response}");
            assert!(completed_marker, "actual Harn assertions must complete: {mode}");
            assert_eq!(execute_count, usize::from(matches!(mode, "normal" | "rewrite")));
        }
        assert_eq!(prepare_count, match mode { "unsupported" => 0, "rewrite" => 2, _ => 1 });
        assert_eq!(release_count, prepare_count);
        assert!(prepared.is_empty() && running.is_empty());
        assert!(!vm_root.path().join("executor-child.json").exists(), "the VM host must not execute the request");
        let marker = executor_root.join("executor-child.json");
        assert_eq!(marker.exists(), matches!(mode, "normal" | "rewrite"));
        if marker.exists() {
            let actual: serde_json::Value = serde_json::from_slice(&std::fs::read(marker).unwrap()).unwrap();
            assert_eq!(actual["cwd"], executor_root.to_string_lossy().as_ref());
            assert_eq!(actual["args"][0], if mode == "rewrite" { "rewritten" } else { " spaced argument " });
            assert_eq!(actual["args"][1], "");
        }
        server.abort();
    }).await;
}
