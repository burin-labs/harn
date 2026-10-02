use super::*;
use std::io::{BufRead, BufReader};

#[test]
fn interrupted_wait_kills_process_group() {
    // Child spawns a grandchild; the whole group must die on interrupt.
    let mut command = std::process::Command::new("sh");
    command.args(["-c", "sleep 30 & printf '%s\\n' \"$!\"; wait"]);
    command.stdout(std::process::Stdio::piped());
    configure_kill_group(&mut command);
    let mut child = command.spawn().expect("spawn sh");
    let pgid = child.id();
    let mut ready = String::new();
    BufReader::new(child.stdout.take().expect("child readiness pipe"))
        .read_line(&mut ready)
        .expect("read descendant PID");
    let descendant: u32 = ready.trim().parse().expect("descendant started");
    assert!(
        process_exists(descendant),
        "descendant must exist before interrupt"
    );

    let cancel = Arc::new(AtomicBool::new(true));
    let _guard = install(Some(cancel), None);
    let started = Instant::now();
    let outcome = wait_child_interruptible(&mut child, None).expect("wait");
    assert!(matches!(outcome, ChildWait::Interrupted(_, _)));
    assert!(started.elapsed() < Duration::from_secs(10));

    // kill(-pgid, 0) fails with ESRCH once every member is gone.
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    let group_gone = || unsafe { kill(-(pgid as i32), 0) } != 0;
    let deadline = Instant::now() + Duration::from_secs(5);
    while !group_gone() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(group_gone(), "process group {pgid} survived interrupt");
}

/// Both halves of the escalation, on one pid each.
///
/// A dead pid on its own cannot tell TERM -> grace -> KILL apart from an
/// immediate KILL: both leave the same corpse. So the polite process
/// writes a marker from inside its SIGTERM handler, and that file is the
/// only evidence that the first signal was ever sent. Reverting
/// `terminate_pid_tree_group_and_token_with_report` to a bare SIGKILL
/// leaves the marker absent; reverting it to a bare SIGTERM leaves the
/// immune pid alive.
#[test]
fn escalating_terminate_kills_a_term_immune_child_and_asks_a_polite_one_first() {
    use std::process::{Command, Stdio};

    let dir = tempfile::tempdir().expect("temp dir");
    let marker = dir.path().join("term-received.marker");

    let mut immune = Command::new("sh")
        .arg("-c")
        .arg("trap '' TERM; printf 'READY\\n'; while true; do sleep 0.05; done")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn term-immune child");
    let mut polite = Command::new("sh")
        .arg("-c")
        .arg(format!(
            "trap 'printf TERM > {}; exit 0' TERM; printf 'READY\\n'; while true; do sleep 0.05; done",
            marker.display()
        ))
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn polite child");

    let immune_pid = immune.id();
    let polite_pid = polite.id();

    // Readiness follows trap installation; a live PID alone does not prove it.
    for child in [&mut immune, &mut polite] {
        let mut ready = String::new();
        BufReader::new(child.stdout.take().expect("child readiness pipe"))
            .read_line(&mut ready)
            .expect("read handler readiness");
        assert_eq!(ready.trim(), "READY", "signal handler must be installed");
    }
    assert!(
        process_exists(immune_pid) && process_exists(polite_pid),
        "both probe children must be running before the terminate"
    );
    assert!(
        !marker.exists(),
        "the SIGTERM marker must not exist before the terminate"
    );

    let immune_report = terminate_pid_tree_group_and_token_with_report(immune_pid, None);
    let polite_report = terminate_pid_tree_group_and_token_with_report(polite_pid, None);

    let _ = immune.wait();
    let _ = polite.wait();

    assert!(
        !process_exists(immune_pid),
        "a child that ignores SIGTERM must still be gone: {immune_report:?}"
    );
    assert!(
        !process_exists(polite_pid),
        "the polite child must be gone: {polite_report:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&marker).unwrap_or_default(),
        "TERM",
        "the polite child must have handled SIGTERM before anything killed it"
    );
    assert!(
        polite_report.attempted_signals.contains(&15),
        "the escalation must record the SIGTERM it sent: {polite_report:?}"
    );
}
