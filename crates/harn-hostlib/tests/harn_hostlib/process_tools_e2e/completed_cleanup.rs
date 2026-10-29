use super::*;

fn descriptor_identity(fd: i32) -> Option<(libc::dev_t, libc::ino_t)> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: fstat initializes the output on success and does not take ownership.
    if unsafe { libc::fstat(fd, stat.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: fstat returned success, so the output is initialized.
    let stat = unsafe { stat.assume_init() };
    Some((stat.st_dev, stat.st_ino))
}

fn inherited_descriptor_identities() -> Vec<(libc::dev_t, libc::ino_t)> {
    std::fs::read_dir("/dev/fd")
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().to_string_lossy().parse::<i32>().ok())
        .filter_map(descriptor_identity)
        .collect()
}

#[test]
fn inherited_descriptor_snapshot_measures_live_pipe_by_fstat() {
    use std::os::fd::AsRawFd;
    let before = inherited_descriptor_identities();
    let (reader, _writer) = std::io::pipe().unwrap();
    let identity = descriptor_identity(reader.as_raw_fd()).unwrap();
    assert!(
        !before.contains(&identity),
        "new pipe must be distinguishable"
    );
    assert!(inherited_descriptor_identities().contains(&identity));
}

#[test]
fn real_completed_background_cancel_replays_guardian_cleanup_receipt() {
    let _session = harn_vm::agent_sessions::enter_current_session(format!(
        "completed-cleanup-{}",
        std::process::id()
    ));
    let python = python3().expect("the process fixture requires Python 3");
    // /dev/fd path metadata is not descriptor identity on macOS. Compare the
    // same fstat values the payload reads, without taking ownership of any fd.
    let inherited_descriptors = inherited_descriptor_identities();
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
    raise AssertionError(f'payload inherited descriptor {name}: dev={stat.st_dev} ino={stat.st_ino} mode={stat.st_mode:o}')
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
