use std::os::unix::fs::PermissionsExt;

fn run(command: &str, cwd: &std::path::Path, background: bool) -> serde_json::Value {
    let request = harn_vm::json_to_vm_value(&serde_json::json!({
        "mode": "shell", "shell_id": "sh", "command": command,
        "cwd": cwd, "env_mode": "replace", "env": {"PATH": "/usr/bin:/bin"},
        "capture": {"final_stage": true}, "background": background,
        "timeout_ms": harn_clock::test_support::HANG_CEILING.as_millis() as u64,
    }));
    let response = super::handle(&[request]).unwrap();
    let response = crate::json::vm_value_to_json(&response);
    if !background {
        return response;
    }
    let handle = response["handle_id"]
        .as_str()
        .expect("actual background handle");
    let crate::tools::long_running::BackgroundWaitOutcome::Completed(terminal) =
        crate::tools::long_running::wait_for_result(handle, harn_clock::test_support::HANG_CEILING)
    else {
        panic!("background clause observation did not finalize");
    };
    crate::json::vm_value_to_json(&terminal)
}

fn executable(path: &std::path::Path, source: &str) {
    std::fs::write(path, source).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

#[test]
fn private_clause_observation_preserves_setup_state_and_short_circuit_failures() {
    for background in [false, true] {
        for failure in [None, Some("prefix"), Some("final")] {
            let root = tempfile::tempdir().unwrap();
            let project = root.path().join("project");
            std::fs::create_dir(&project).unwrap();
            std::fs::write(project.join("present"), "fixture").unwrap();
            executable(
                &root.path().join("prepare"),
                "#!/bin/sh\nprintf setup\nprintf reached > prepared\n",
            );
            executable(
                &project.join("verify"),
                "#!/bin/sh\nprintf reached > entered\nprintf 'stage:%s' \"$CHECK_MODE\"\n",
            );
            executable(
                &root.path().join("fail"),
                "#!/bin/sh\nprintf failure\nexit 7\n",
            );
            let command = match failure {
                None => "./prepare && export CHECK_MODE=fixture && cd project && test -f present && [ -f present ] && ./verify",
                Some("prefix") => "./prepare && [ -f missing ] && ./fail",
                _ => "./prepare && ./fail",
            };
            let response = run(command, root.path(), background);
            assert_eq!(
                std::fs::read_to_string(root.path().join("prepared")).unwrap(),
                "reached"
            );
            let stage = &response["stage_output"];
            match failure {
                None => {
                    assert_eq!(response["exit_code"], 0, "{response}");
                    assert_eq!(
                        std::fs::read_to_string(project.join("entered")).unwrap(),
                        "reached"
                    );
                    assert_eq!(stage["status"], "captured", "{response}");
                    assert_eq!(stage["stdout"], "stage:fixture");
                    assert!(stage.get("failed_clause").is_none());
                    assert_eq!(response["stdout"], "setupstage:fixture");
                }
                Some("prefix") => {
                    assert_eq!(response["exit_code"], 1, "{response}");
                    assert_eq!(stage["status"], "unmeasured");
                    assert_eq!(stage["reason"], "final_stage_unobserved");
                    assert_eq!(
                        stage["failed_clause"],
                        serde_json::json!({"index": 2, "total": 3, "source": "[ -f missing ]"})
                    );
                    assert_eq!(response["stdout"], "setup");
                    assert!(!project.join("entered").exists());
                }
                _ => {
                    assert_eq!(response["exit_code"], 7, "{response}");
                    assert_eq!(stage["status"], "captured");
                    assert_eq!(stage["stdout"], "failure");
                    assert_eq!(
                        stage["failed_clause"],
                        serde_json::json!({"index": 2, "total": 2, "source": "./fail"})
                    );
                    assert_eq!(response["stdout"], "setupfailure");
                }
            }
        }
    }
}

#[test]
fn unsupported_capture_keeps_opted_in_diagnostics_without_reexecution_or_attribution() {
    for (startup, extended_args) in [(false, false), (true, false), (true, true)] {
        let root = tempfile::tempdir().unwrap();
        executable(
            &root.path().join("prepare"),
            "#!/bin/sh\nprintf reached >> prepared\nprintf setup\n",
        );
        executable(
            &root.path().join("fail"),
            "#!/bin/sh\nprintf reached >> failed\nprintf failure\nexit 7\n",
        );
        let startup_file = root.path().join("startup.sh");
        std::fs::write(&startup_file, "printf started > startup-reached\n").unwrap();
        let env = if startup {
            serde_json::json!({"PATH": "/usr/bin:/bin", "HOME": root.path(), "BASH_ENV": startup_file})
        } else {
            serde_json::json!({"PATH": "/usr/bin:/bin"})
        };
        let diagnostics = [
            "setup context",
            "VERIFY CLAUSE FAILED [2/2]: ./fail\nfile guard guidance: '$HOME'; $(touch injected)\n",
        ];
        let shell = if extended_args {
            serde_json::json!({"id": "bash", "path": "/bin/bash", "available": true,
                "default_args": ["--noprofile", "--norc", "-l", "-c"]})
        } else {
            serde_json::Value::Null
        };
        let request = harn_vm::json_to_vm_value(&serde_json::json!({
            "mode": "shell", "shell_id": "bash", "command": "./prepare && ./fail",
            "shell": shell,
            "cwd": root.path(), "env_mode": "replace", "env": env,
            "capture": {"final_stage": true, "clause_diagnostics": diagnostics, "transport": if startup {"pipe"} else {"file"}},
            "timeout_ms": harn_clock::test_support::HANG_CEILING.as_millis() as u64,
        }));
        let response = crate::json::vm_value_to_json(&super::handle(&[request]).unwrap());
        assert_eq!(response["exit_code"], 7, "{response}");
        assert_eq!(
            std::fs::read_to_string(root.path().join("prepared")).unwrap(),
            "reached"
        );
        assert_eq!(
            std::fs::read_to_string(root.path().join("failed")).unwrap(),
            "reached"
        );
        assert_eq!(root.path().join("startup-reached").exists(), startup);
        assert!(
            response["stdout"]
                .as_str()
                .unwrap()
                .contains(diagnostics[1]),
            "{response}"
        );
        assert_eq!(response["stage_output"]["status"], "unmeasured");
        assert!(response["stage_output"].get("failed_clause").is_none());
        assert!(
            !root.path().join("injected").exists(),
            "diagnostic text must not execute shell syntax"
        );
    }
}

#[test]
fn malformed_diagnostic_projection_refuses_before_command_entry() {
    let root = tempfile::tempdir().unwrap();
    executable(
        &root.path().join("prepare"),
        "#!/bin/sh\nprintf entered >> entries\n",
    );
    executable(&root.path().join("verify"), "#!/bin/sh\nprintf verifier\n");
    let request = |diagnostics: serde_json::Value| {
        harn_vm::json_to_vm_value(&serde_json::json!({
            "mode": "shell", "shell_id": "sh", "command": "./prepare && ./verify",
            "cwd": root.path(), "env_mode": "replace", "env": {"PATH": "/usr/bin:/bin"},
            "capture": {"final_stage": true, "clause_diagnostics": diagnostics},
            "timeout_ms": harn_clock::test_support::HANG_CEILING.as_millis() as u64,
        }))
    };
    let positive = crate::json::vm_value_to_json(
        &super::handle(&[request(serde_json::json!(["setup", "verifier"]))]).unwrap(),
    );
    assert_eq!(positive["exit_code"], 0, "{positive}");
    assert_eq!(positive["stage_output"]["stdout"], "verifier");
    assert_eq!(
        std::fs::read_to_string(root.path().join("entries")).unwrap(),
        "entered"
    );
    for diagnostics in [
        serde_json::json!(["only one"]),
        serde_json::json!([1, 2]),
        serde_json::json!(true),
    ] {
        assert!(super::handle(&[request(diagnostics)]).is_err());
        assert_eq!(
            std::fs::read_to_string(root.path().join("entries")).unwrap(),
            "entered",
            "invalid projection must not execute the prefix"
        );
    }
}
