use super::*;

#[test]
fn run_command_runs_in_supplied_cwd() {
    let (spawner, _controller, _guard) = install_mock_with(MockProcessConfig::completed(0));

    let dir = tempdir().unwrap();
    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["bash", "-c", "pwd"]));
    req.insert("cwd".into(), vstr(dir.path().to_str().unwrap()));
    let resp = require_dict(call("hostlib_tools_run_command", req).unwrap());

    assert_eq!(require_int(&resp, "exit_code"), 0);
    let captured = spawner.captured();
    assert_eq!(captured.len(), 1);
    let canon_cwd = std::fs::canonicalize(dir.path()).unwrap();
    assert_eq!(captured[0].cwd.as_ref().unwrap(), &canon_cwd);
    assert_eq!(require_str(&resp, "cwd"), canon_cwd.to_string_lossy());
}

#[test]
fn run_command_reports_resolved_inherited_cwd() {
    let (spawner, _controller, _guard) = install_mock_with(MockProcessConfig::completed(0));
    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["true"]));

    let resp = require_dict(call("hostlib_tools_run_command", req).unwrap());
    let expected = std::env::current_dir().unwrap().canonicalize().unwrap();

    assert_eq!(require_str(&resp, "cwd"), expected.to_string_lossy());
    assert_eq!(spawner.captured()[0].cwd.as_ref(), Some(&expected));
}

#[test]
fn run_command_inherits_the_vm_execution_cwd_not_the_host_process_cwd() {
    let (spawner, _controller, _guard) = install_mock_with(MockProcessConfig::completed(0));
    let execution_dir = tempdir().unwrap();
    let expected = execution_dir.path().canonicalize().unwrap();
    harn_vm::stdlib::process::set_thread_execution_context(Some(
        harn_vm::orchestration::RunExecutionRecord {
            cwd: Some(expected.to_string_lossy().into_owned()),
            ..Default::default()
        },
    ));

    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["true"]));
    let result = call("hostlib_tools_run_command", req);
    harn_vm::stdlib::process::set_thread_execution_context(None);
    let resp = require_dict(result.unwrap());

    assert_eq!(require_str(&resp, "cwd"), expected.to_string_lossy());
    assert_eq!(spawner.captured()[0].cwd.as_ref(), Some(&expected));
}
