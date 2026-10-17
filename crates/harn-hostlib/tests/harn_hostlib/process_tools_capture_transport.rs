//! Focused mock coverage for `run_command` capture transports.
//!
//! The larger `process_tools.rs` suite owns the general process-tool matrix.
//! This file keeps the file-backed capture contract isolated so the
//! existing suite does not grow every time a transport is added.

#![cfg(unix)]

use harn_hostlib::process::{ExitStatus, MockProcessConfig, OutputCapture};
use harn_hostlib::HostlibError;
use harn_vm::VmValue;
use sha2::{Digest, Sha256};

use super::support::*;

fn capture_with_options(
    transport: Option<&str>,
    merge_stderr: bool,
    max_inline_bytes: i64,
) -> harn_vm::value::DictMap {
    let mut capture = dict_map();
    capture.insert("max_inline_bytes".into(), VmValue::Int(max_inline_bytes));
    if let Some(transport) = transport {
        capture.insert("transport".into(), vm_string(transport));
    }
    if merge_stderr {
        capture.insert("merge_stderr".into(), VmValue::Bool(true));
    }
    capture
}

fn run_completed_capture(
    transport: Option<&str>,
    merge_stderr: bool,
    max_inline_bytes: i64,
) -> (harn_vm::value::DictMap, OutputCapture) {
    let config = MockProcessConfig {
        stdout: b"alpha-stdout\n".to_vec(),
        stderr: b"bravo-stderr\n".to_vec(),
        exit_status: Some(ExitStatus::from_code(0)),
        ..MockProcessConfig::default()
    };
    let (spawner, _guard) = install_mock();
    spawner.enqueue(config);

    let mut req = dict_map();
    req.insert("argv".into(), vlist_str(&["fixture"]));
    req.insert(
        "capture".into(),
        VmValue::dict(capture_with_options(
            transport,
            merge_stderr,
            max_inline_bytes,
        )),
    );
    let resp_value = call_tool("hostlib_tools_run_command", req).unwrap();
    assert_response_matches_schema("run_command", &resp_value);
    let resp = require_dict(resp_value);
    let captured = spawner.captured();
    assert_eq!(captured.len(), 1);
    (resp, captured[0].output_capture.clone())
}

fn assert_path_bytes(map: &harn_vm::value::DictMap, key: &str, expected: &[u8]) {
    let path = require_str(map, key);
    assert_eq!(std::fs::read(path).unwrap(), expected);
}

fn assert_output_contract_pair(
    pipe: &harn_vm::value::DictMap,
    file: &harn_vm::value::DictMap,
    expected_stdout: &str,
    expected_stderr: &str,
) {
    assert_eq!(
        require_int(pipe, "exit_code"),
        require_int(file, "exit_code")
    );
    assert_eq!(
        require_bool(pipe, "timed_out"),
        require_bool(file, "timed_out")
    );
    assert_eq!(require_str(pipe, "status"), require_str(file, "status"));
    assert_eq!(require_str(pipe, "stdout"), require_str(file, "stdout"));
    assert_eq!(require_str(pipe, "stderr"), require_str(file, "stderr"));
    assert_eq!(
        require_int(pipe, "line_count"),
        require_int(file, "line_count")
    );
    assert_eq!(
        require_int(pipe, "byte_count"),
        require_int(file, "byte_count")
    );
    assert_eq!(
        require_str(pipe, "output_sha256"),
        require_str(file, "output_sha256")
    );
    assert_eq!(require_str(file, "stdout"), expected_stdout);
    assert_eq!(require_str(file, "stderr"), expected_stderr);

    let stdout = b"alpha-stdout\n";
    let stderr = b"bravo-stderr\n";
    let mut combined = Vec::new();
    combined.extend_from_slice(stdout);
    combined.extend_from_slice(stderr);
    assert_path_bytes(file, "stdout_path", stdout);
    assert_path_bytes(file, "stderr_path", stderr);
    assert_path_bytes(file, "output_path", &combined);
    let digest = format!("sha256:{}", hex::encode(Sha256::digest(&combined)));
    assert_eq!(require_str(file, "output_sha256"), digest);
}

#[test]
fn run_command_file_capture_returns_when_descendant_keeps_stdout_open() {
    let config = MockProcessConfig {
        stdout: b"direct-child-output\n".to_vec(),
        exit_status: Some(ExitStatus::from_code(0)),
        stdout_hangs_after_exit_until_kill: true,
        ..MockProcessConfig::default()
    };
    let (spawner, _guard) = install_mock();
    let controller = spawner.enqueue(config);

    let mut capture: harn_vm::value::DictMap = Default::default();
    capture.insert("transport".into(), vm_string("file"));

    let mut req = dict_map();
    req.insert("argv".into(), vlist_str(&["python3", "build-daemon.py"]));
    req.insert("timeout_ms".into(), VmValue::Int(1));
    req.insert("capture".into(), VmValue::dict(capture));
    let resp_value = call_tool("hostlib_tools_run_command", req).unwrap();
    assert_response_matches_schema("run_command", &resp_value);
    let resp = require_dict(resp_value);

    assert!(!controller.was_killed());
    assert!(!require_bool(&resp, "timed_out"));
    assert_eq!(require_str(&resp, "status"), "completed");
    assert_eq!(require_int(&resp, "exit_code"), 0);
    assert_eq!(require_str(&resp, "stdout"), "direct-child-output\n");
    assert!(resp.get("process_cleanup").is_none());
    let captured = spawner.captured();
    assert_eq!(captured.len(), 1);
    assert!(
        matches!(&captured[0].output_capture, OutputCapture::File { .. }),
        "file capture should attach temp files instead of pipes"
    );
}

#[test]
fn run_command_file_capture_matches_pipe_capture_output_contract() {
    let (pipe, pipe_capture) = run_completed_capture(None, false, 5);
    let (file, file_capture) = run_completed_capture(Some("file"), false, 5);
    assert!(matches!(pipe_capture, OutputCapture::Pipe));
    assert!(matches!(file_capture, OutputCapture::File { .. }));
    assert_output_contract_pair(&pipe, &file, "alpha", "bravo");

    let (pipe_merged, pipe_merged_capture) = run_completed_capture(None, true, 64);
    let (file_merged, file_merged_capture) = run_completed_capture(Some("file"), true, 64);
    assert!(matches!(pipe_merged_capture, OutputCapture::Pipe));
    assert!(matches!(file_merged_capture, OutputCapture::File { .. }));
    assert_output_contract_pair(
        &pipe_merged,
        &file_merged,
        "alpha-stdout\nbravo-stderr\n",
        "",
    );
}

#[test]
fn run_command_rejects_unknown_capture_transport() {
    let mut capture: harn_vm::value::DictMap = Default::default();
    capture.insert("transport".into(), vm_string("socket"));

    let mut req = dict_map();
    req.insert("argv".into(), vlist_str(&["true"]));
    req.insert("capture".into(), VmValue::dict(capture));

    let err = call_tool("hostlib_tools_run_command", req).unwrap_err();
    assert!(
        matches!(err, HostlibError::InvalidParameter { param, .. } if param == "capture.transport")
    );
}

#[test]
fn run_command_rejects_file_capture_for_background_progress() {
    let mut capture: harn_vm::value::DictMap = Default::default();
    capture.insert("transport".into(), vm_string("file"));

    let mut req = dict_map();
    req.insert("argv".into(), vlist_str(&["true"]));
    req.insert("background".into(), VmValue::Bool(true));
    req.insert("capture".into(), VmValue::dict(capture));

    let err = call_tool("hostlib_tools_run_command", req).unwrap_err();
    assert!(
        matches!(err, HostlibError::InvalidParameter { param, .. } if param == "capture.transport")
    );
}
