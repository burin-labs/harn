use super::*;

#[cfg(target_os = "linux")]
#[test]
fn active_cancel_preserves_guardian_custody_for_scrubbed_detached_child() {
    active_cleanup(false);
}

#[cfg(target_os = "linux")]
#[test]
fn active_deadline_preserves_guardian_custody_for_scrubbed_detached_child() {
    active_cleanup(true);
}

#[cfg(target_os = "linux")]
fn active_cleanup(deadline: bool) {
    use std::os::fd::AsRawFd;
    let _session = harn_vm::agent_sessions::enter_current_session(format!(
        "cancel-cleanup-{}-{deadline}",
        std::process::id(),
    ));
    let python = python3().expect("the process fixture requires Python 3");
    let temp = tempfile::tempdir().unwrap();
    let fifo = temp.path().join("ready.fifo");
    assert!(std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap()
        .success());
    let mut ready = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&fifo)
        .unwrap();
    let script = r#"
import os, signal, sys
r, w = os.pipe()
pid = os.fork()
if pid == 0:
    os.close(r)
    os.setsid()
    os.set_inheritable(w, True)
    os.execve(sys.executable, [sys.executable, '-c',
        'import os,signal,sys; os.write(int(sys.argv[1]),b"R"); os.close(int(sys.argv[1])); signal.pause()', str(w)], {})
os.close(w)
assert os.read(r, 1) == b'R'
os.close(r)
signal.signal(signal.SIGTERM, signal.SIG_IGN)
with open(sys.argv[1], 'w') as ready:
    ready.write(str(pid) + '\n')
    ready.flush()
while True: signal.pause()
"#;
    let mut request = dict_map();
    request.insert(
        "argv".into(),
        vlist_str(&[&python, "-c", script, &fifo.to_string_lossy()]),
    );
    request.put_bool("background", true);
    if deadline {
        request.put_int("hard_timeout_ms", 10_000);
    }
    let started = require_dict(call("hostlib_tools_run_command", request).unwrap());
    let handle = require_str(&started, "handle_id");
    let mut bytes = Vec::new();
    loop {
        let mut signal = libc::pollfd {
            fd: ready.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        assert!(
            unsafe { libc::poll(&raw mut signal, 1, 15_000) } > 0,
            "detached child did not signal readiness"
        );
        let mut byte = [0_u8; 1];
        ready.read_exact(&mut byte).unwrap();
        if byte[0] == b'\n' {
            break;
        }
        bytes.push(byte[0]);
    }
    let pid: i32 = std::str::from_utf8(&bytes).unwrap().parse().unwrap();
    assert!(
        pid > 1 && unsafe { libc::kill(pid, 0) } == 0,
        "detached child must exist before cancellation"
    );
    let pid_path = temp.path().join("detached.pid");
    std::fs::write(&pid_path, pid.to_string()).unwrap();
    let _cleanup = PidFileCleanup { path: pid_path };
    let completed =
        harn_hostlib::tools::long_running::register_completion_notifier(&handle).unwrap();
    if !deadline {
        let mut cancel = dict_map();
        cancel.insert("handle_id".into(), vm_string(&handle));
        assert!(require_bool(
            &require_dict(call("hostlib_tools_cancel_handle", cancel).unwrap()),
            "cancelled"
        ));
    }
    harn_clock::test_support::recv_within("cancelled handle settled", &completed);
    let mut wait = dict_map();
    wait.insert("handle_id".into(), vm_string(&handle));
    let terminal = require_dict(call("hostlib_tools_wait_command", wait).unwrap());
    assert_eq!(
        require_str(&terminal, "status"),
        if deadline { "timed_out" } else { "killed" }
    );
    assert_eq!(require_bool(&terminal, "timed_out"), deadline);
    let cleanup = require_nested_dict(&terminal, "process_cleanup");
    assert!(require_bool(&cleanup, "guardian_custody_complete"));
    assert_eq!(require_int(&cleanup, "survivor_count"), 0);
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1, "scrubbed child survived");
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
}

#[cfg(target_os = "linux")]
#[test]
fn guardian_reaps_detached_child_without_owner_environment() {
    let _session = harn_vm::agent_sessions::enter_current_session(format!(
        "scrubbed-cleanup-{}",
        std::process::id()
    ));
    let python = python3().expect("the process fixture requires Python 3");
    let script = r#"
import os, sys
r, w = os.pipe()
pid = os.fork()
if pid == 0:
    os.close(r)
    os.setsid()
    os.set_inheritable(w, True)
    os.execve(sys.executable, [sys.executable, '-c',
        'import os,signal,sys; os.write(int(sys.argv[1]),b"R"); os.close(int(sys.argv[1])); signal.pause()', str(w)], {})
os.close(w)
assert os.read(r, 1) == b'R'
os.close(r)
print('SCRUBBED_PID=' + str(pid), flush=True)
"#;
    let mut request = dict_map();
    request.insert("argv".into(), vlist_str(&[&python, "-c", script]));
    request.put_bool("background", true);
    let started = require_dict(call("hostlib_tools_run_command", request).unwrap());
    let mut wait = dict_map();
    wait.insert(
        "handle_id".into(),
        vm_string(&require_str(&started, "handle_id")),
    );
    wait.put_int("timeout_ms", 15_000);
    let result = call("hostlib_tools_wait_command", wait).unwrap();
    assert_response_matches_schema("wait_command", &result);
    let result = require_dict(result);
    assert_eq!(require_str(&result, "status"), "completed");
    assert_eq!(require_int(&result, "exit_code"), 0);
    let stdout = require_str(&result, "stdout");
    let pid: i32 = stdout
        .lines()
        .find_map(|line| line.trim().strip_prefix("SCRUBBED_PID=")?.parse().ok())
        .unwrap_or_else(|| panic!("scrubbed child did not report its PID: {stdout:?}"));
    assert!(pid > 0);
    let cleanup = require_nested_dict(&result, "process_cleanup");
    assert!(require_bool(&cleanup, "guardian_custody_complete"));
    assert_eq!(require_int(&cleanup, "survivor_count"), 0);
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1, "scrubbed child survived");
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
}

#[test]
fn real_completed_background_cancel_replays_guardian_cleanup_receipt() {
    let _session = harn_vm::agent_sessions::enter_current_session(format!(
        "completed-cleanup-{}",
        std::process::id()
    ));
    let python = python3().expect("the process fixture requires Python 3");
    use std::os::unix::fs::MetadataExt;
    let inherited_descriptors = std::fs::read_dir("/dev/fd")
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|entry| std::fs::metadata(entry.path()).ok())
        .map(|metadata| (metadata.dev(), metadata.ino()))
        .collect::<Vec<_>>();
    let script = r"
import errno, os, signal
inherited_descriptors = __INHERITED_DESCRIPTORS__
for name in os.listdir('/dev/fd'):
    fd = int(name)
    if fd <= 2: continue
    try:
        stat = os.fstat(fd)
    except OSError as error:
        assert error.errno == errno.EBADF
        continue
    if [stat.st_dev, stat.st_ino] in inherited_descriptors: continue
    raise AssertionError('payload inherited descriptor ' + name + ': ' + os.readlink('/dev/fd/' + name))
r, w = os.pipe()
pid = os.fork()
if pid == 0:
    os.close(r)
    os.setsid()
    os.close(0)
    os.close(1)
    os.close(2)
    os.write(w, b'R')
    os.close(w)
    while True: signal.pause()
os.close(w)
assert os.read(r, 1) == b'R'
os.close(r)
print(pid, flush=True)
"
    .replace(
        "__INHERITED_DESCRIPTORS__",
        &serde_json::to_string(&inherited_descriptors).unwrap(),
    );
    let mut request = dict_map();
    request.insert("argv".into(), vlist_str(&[&python, "-c", &script]));
    request.put_bool("background", true);
    let started = require_dict(call("hostlib_tools_run_command", request).unwrap());
    let handle = require_str(&started, "handle_id");
    let mut wait = dict_map();
    wait.insert("handle_id".into(), vm_string(&handle));
    wait.put_int("timeout_ms", 15_000);
    let waited = call("hostlib_tools_wait_command", wait).unwrap();
    assert_response_matches_schema("wait_command", &waited);
    let waited = require_dict(waited);
    assert_eq!(require_str(&waited, "status"), "completed");
    assert_eq!(
        require_int(&waited, "exit_code"),
        0,
        "payload stderr: {}",
        require_str(&waited, "stderr")
    );
    let stdout = require_str(&waited, "stdout");
    let pid: i32 = stdout
        .lines()
        .find_map(|line| line.trim().parse().ok())
        .unwrap_or_else(|| panic!("detached child did not report its PID: {stdout:?}"));
    assert!(pid > 0);
    let cleanup = require_nested_dict(&waited, "process_cleanup");
    assert!(require_int(&cleanup, "observed_child_count") > 0);
    let children = require_list(&cleanup, "observed_children");
    let child = children
        .iter()
        .map(as_dict)
        .find(|child| require_int(child, "pid") == i64::from(pid))
        .expect("receipt must measure the actual detached child");
    assert!(!require_bool(&child, "alive_after_cleanup"));
    assert_no_observed_survivors(&cleanup);
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1, "detached child survived");
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
    let mut cancel = dict_map();
    cancel.insert("handle_id".into(), vm_string(&handle));
    cancel.put_int("wait_result_ms", 15_000);
    let cancelled = require_dict(call("hostlib_tools_cancel_handle", cancel).unwrap());
    assert!(!require_bool(&cancelled, "cancelled"));
    let replay = require_nested_dict(&cancelled, "result");
    assert_eq!(
        harn_vm::value::json::vm_value_dict_to_json(&cleanup),
        harn_vm::value::json::vm_value_dict_to_json(&require_nested_dict(
            &replay,
            "process_cleanup"
        ))
    );
    assert_eq!(
        require_str(&replay, "command_id"),
        require_str(&waited, "command_id")
    );
}

pub(super) fn assert_no_observed_survivors(cleanup: &harn_vm::value::DictMap) {
    assert!(require_list(cleanup, "surviving_children").is_empty());
    if require_bool(cleanup, "census_complete") {
        assert!(require_bool(cleanup, "census_control_read"));
        assert!(require_int(cleanup, "census_scanned_processes") > 0);
        assert_eq!(require_int(cleanup, "survivor_count"), 0);
    } else if require_bool(cleanup, "guardian_custody_complete") {
        assert_eq!(require_int(cleanup, "omitted_child_count"), 0);
        assert_eq!(require_int(cleanup, "survivor_count"), 0);
    } else {
        assert!(matches!(cleanup.get("survivor_count"), Some(VmValue::Nil)));
        assert!(
            !require_bool(cleanup, "census_control_read")
                || require_int(cleanup, "census_scanned_processes") == 0
                || require_int(cleanup, "census_unreadable_processes") > 0
                || require_int(cleanup, "omitted_child_count") > 0
        );
    }
}
