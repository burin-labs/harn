use super::*;

fn assert_missing_process_error(error: HostlibError, expected_requested_cwd: Option<&str>) {
    match error {
        HostlibError::ProcessSpawn {
            kind,
            message,
            requested_cwd,
            cwd,
            ..
        } => {
            assert_eq!(kind, "not_found");
            assert!(!message.is_empty());
            assert_eq!(requested_cwd.as_deref(), expected_requested_cwd);
            assert!(!cwd.is_empty());
        }
        other => panic!("expected typed process-spawn error, got {other:?}"),
    }
}

#[test]
fn real_run_command_reports_missing_program_as_typed_not_found() {
    let workspace = tempfile::tempdir().unwrap();
    let cwd = workspace.path().canonicalize().unwrap();
    let cwd = cwd.to_str().unwrap();
    for background in [false, true] {
        for requested_cwd in [None, Some(cwd)] {
            let mut req = dict_map();
            req.insert(
                "argv".into(),
                vlist_str(&["harn-command-that-does-not-exist-5662"]),
            );
            req.insert("background".into(), VmValue::Bool(background));
            if let Some(cwd) = requested_cwd {
                req.insert("cwd".into(), vm_string(cwd));
            }
            let error =
                call("hostlib_tools_run_command", req).expect_err("missing program must fail");
            assert_missing_process_error(error, requested_cwd);
        }
    }
}

#[test]
fn real_run_command_classifies_missing_program_without_stderr_matching() {
    let workspace = tempfile::tempdir().unwrap();
    let missing = "harn-absent-program-8932";
    for command in [
        missing.to_string(),
        format!("{missing} && true"),
        format!("env VAR=1 {missing}"),
        format!("VAR=1 {missing} > missing.out"),
        "./relative/absent-8932".to_string(),
    ] {
        let mut req = dict_map();
        req.insert("mode".into(), vm_string("shell"));
        req.insert("shell_id".into(), vm_string("sh"));
        req.insert("command".into(), vm_string(&command));
        req.insert("cwd".into(), vm_string(workspace.path().to_str().unwrap()));
        let response = require_dict(call("hostlib_tools_run_command", req).unwrap());
        assert_eq!(require_int(&response, "exit_code"), 127, "{command}");
        let expected = if command.starts_with("./") {
            command.as_str()
        } else {
            missing
        };
        assert_eq!(
            require_str(&response, "missing_program"),
            expected,
            "{command}"
        );
    }
}

#[test]
fn real_run_command_present_program_exit_127_is_not_missing() {
    use std::os::unix::fs::PermissionsExt;
    let workspace = tempfile::tempdir().unwrap();
    let program = workspace.path().join("present-8932");
    std::fs::write(&program, "#!/bin/sh\nexit 127\n").unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    for (mode, command) in [
        ("argv", ""),
        ("shell", "env VAR=1 present-8932"),
        ("shell", "PATH=~ present-8932"),
        ("auto", "env VAR=1 present-8932"),
        ("auto", "present-8932"),
    ] {
        let mut req = dict_map();
        req.insert("mode".into(), vm_string(mode));
        req.insert("cwd".into(), vm_string(workspace.path().to_str().unwrap()));
        let mut env = dict_map();
        env.insert("PATH".into(), vm_string(workspace.path().to_str().unwrap()));
        req.put("env", VmValue::dict(env));
        if mode == "argv" {
            req.insert("argv".into(), vlist_str(&[program.to_str().unwrap()]));
        } else {
            req.insert("shell_id".into(), vm_string("sh"));
            req.insert("command".into(), vm_string(command));
            // env itself must remain available in the child PATH.
            let mut env = dict_map();
            env.insert(
                "PATH".into(),
                vm_string(&format!("{}:/usr/bin:/bin", workspace.path().display())),
            );
            env.insert("HOME".into(), vm_string(workspace.path().to_str().unwrap()));
            req.put("env", VmValue::dict(env));
        }
        let response = require_dict(call("hostlib_tools_run_command", req).unwrap());
        assert_eq!(require_int(&response, "exit_code"), 127, "{mode}");
        assert!(!response.contains_key("missing_program"), "{mode}");
    }
}

#[test]
fn real_run_command_startup_function_exit_127_is_not_missing() {
    let workspace = tempfile::tempdir().unwrap();
    let startup = workspace.path().join("startup.sh");
    std::fs::write(
        &startup,
        "present_startup_8932() { printf 'called\\n' > called; return 127; }\n",
    )
    .unwrap();
    let mut req = dict_map();
    req.insert("mode".into(), vm_string("shell"));
    req.insert("shell_id".into(), vm_string("bash"));
    req.insert("command".into(), vm_string("present_startup_8932"));
    req.insert("cwd".into(), vm_string(workspace.path().to_str().unwrap()));
    let mut env = dict_map();
    env.insert("BASH_ENV".into(), vm_string(startup.to_str().unwrap()));
    req.put("env", VmValue::dict(env));
    let response = require_dict(call("hostlib_tools_run_command", req).unwrap());
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("called")).unwrap(),
        "called\n"
    );
    assert_eq!(require_int(&response, "exit_code"), 127);
    assert!(!response.contains_key("missing_program"));
}
