//! Build parser results retain terminal and capture completeness facts.
use super::*;

#[test]
fn run_build_command_explicit_argv_runs_and_parses_diagnostics() {
    let config = MockProcessConfig {
        stderr: b"src/foo.rs:3:7: error: parse error here\n".to_vec(),
        ..MockProcessConfig::completed(2)
    };
    let (_spawner, _controller, _guard) = install_mock_with(config);
    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["compiler", "source"]));
    let resp = require_dict(call("hostlib_tools_run_build_command", req).unwrap());
    assert_eq!(require_int(&resp, "exit_code"), 2);
    assert_eq!(require_str(&resp, "status"), "completed");
    assert!(require_bool(&resp, "diagnostics_complete"));
    let diagnostics = match resp.get("diagnostics") {
        Some(VmValue::List(l)) => l,
        other => panic!("expected list, got {other:?}"),
    };
    assert_eq!(diagnostics.len(), 1);
    let diagnostic = require_dict(diagnostics[0].clone());
    assert_eq!(require_str(&diagnostic, "message"), "parse error here");
    assert_eq!(require_str(&diagnostic, "severity"), "error");
}

#[test]
fn partial_and_timed_out_builds_cannot_attest_complete_diagnostics() {
    for timed_out in [false, true] {
        let config = MockProcessConfig {
            stderr: if timed_out {
                b"error: first\n".to_vec()
            } else {
                b"error: repeated\n".repeat(4000)
            },
            force_timeout: timed_out,
            ..MockProcessConfig::completed(1)
        };
        let (_spawner, _controller, _guard) = install_mock_with(config);
        let mut req = dict();
        req.insert("argv".into(), vlist_str(&["compiler", "source"]));
        req.insert("timeout_ms".into(), VmValue::Int(1000));
        let resp = require_dict(call("hostlib_tools_run_build_command", req).unwrap());
        assert!(!require_bool(&resp, "diagnostics_complete"));
        assert_eq!(require_bool(&resp, "timed_out"), timed_out);
        assert!(
            matches!(resp.get("diagnostics"), Some(VmValue::List(items)) if !items.is_empty()),
            "partial evidence must still be parsed and visible"
        );
    }
}

#[test]
fn run_build_command_without_argv_or_manifest_errors() {
    let dir = tempdir().unwrap();
    let mut req = dict();
    req.insert("cwd".into(), vstr(dir.path().to_str().unwrap()));
    let err = call("hostlib_tools_run_build_command", req).unwrap_err();
    assert!(matches!(err, HostlibError::InvalidParameter { param, .. } if param == "argv"));
}
