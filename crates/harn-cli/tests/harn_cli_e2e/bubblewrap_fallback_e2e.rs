#![cfg(target_os = "linux")]

use std::os::unix::process::CommandExt;
use std::process::Command;

use crate::test_util::process::harn_e2e_command;

/// Deny only the kernel facilities under test in the launched CLI. No global
/// kernel setting or backend selector can make a prototype pass this proof.
fn deny_landlock(command: &mut Command, deny_namespaces: bool) {
    let mut instructions = vec![libc::sock_filter {
        code: (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16,
        jt: 0,
        jf: 0,
        k: 0,
    }];
    for syscall in [
        Some(libc::SYS_landlock_create_ruleset),
        deny_namespaces.then_some(libc::SYS_unshare),
        deny_namespaces.then_some(libc::SYS_clone3),
    ]
    .into_iter()
    .flatten()
    {
        instructions.push(libc::sock_filter {
            code: (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
            jt: 0,
            jf: 1,
            k: syscall as u32,
        });
        instructions.push(libc::sock_filter {
            code: (libc::BPF_RET | libc::BPF_K) as u16,
            jt: 0,
            jf: 0,
            k: libc::SECCOMP_RET_ERRNO | libc::ENOSYS as u32,
        });
    }
    if deny_namespaces {
        // Normal fork/thread clone calls remain available. clone3 returns
        // ENOSYS so libc can use its ordinary clone fallback; namespace-bearing
        // clone flags are refused at the actual kernel argument boundary.
        instructions.extend([
            libc::sock_filter {
                code: (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
                jt: 0,
                jf: 3,
                k: libc::SYS_clone as u32,
            },
            libc::sock_filter {
                code: (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16,
                jt: 0,
                jf: 0,
                k: 16,
            },
            libc::sock_filter {
                code: (libc::BPF_JMP | libc::BPF_JSET | libc::BPF_K) as u16,
                jt: 0,
                jf: 1,
                k: (libc::CLONE_NEWUSER
                    | libc::CLONE_NEWNS
                    | libc::CLONE_NEWNET
                    | libc::CLONE_NEWPID
                    | libc::CLONE_NEWIPC) as u32,
            },
            libc::sock_filter {
                code: (libc::BPF_RET | libc::BPF_K) as u16,
                jt: 0,
                jf: 0,
                k: libc::SECCOMP_RET_ERRNO | libc::ENOSYS as u32,
            },
        ]);
    }
    instructions.push(libc::sock_filter {
        code: (libc::BPF_RET | libc::BPF_K) as u16,
        jt: 0,
        jf: 0,
        k: libc::SECCOMP_RET_ALLOW,
    });
    // SAFETY: the closure owns its preallocated instructions and performs only
    // prctl syscalls between fork and exec.
    unsafe {
        command.pre_exec(move || {
            let program = libc::sock_fprog {
                len: instructions.len() as u16,
                filter: instructions.as_ptr().cast_mut(),
            };
            if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
                || libc::prctl(libc::PR_SET_SECCOMP, libc::SECCOMP_MODE_FILTER, &program) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[test]
fn canonical_child_uses_bubblewrap_and_preserves_filesystem_network_and_syscall_grants() {
    let tree = tempfile::tempdir().unwrap();
    let root = tree.path().join("workspace");
    let readonly = tree.path().join("readonly");
    let home = tree.path().join("home");
    for path in [&root, &readonly, &home.join(".cargo")] {
        std::fs::create_dir_all(path).unwrap();
    }
    let credential = home.join(".cargo/credentials.toml");
    let alias = home.join("credential-alias");
    std::fs::write(&credential, "synthetic-denied").unwrap();
    std::os::unix::fs::symlink(&credential, &alias).unwrap();
    assert_eq!(std::fs::read(&alias).unwrap(), b"synthetic-denied");
    std::fs::write(readonly.join("marker"), "readonly").unwrap();
    let outside = tree.path().join("outside");
    std::fs::write(&outside, "known-outside").unwrap();
    let probe = format!(
        r"
import ctypes, errno, os, socket, tempfile
assert open({readonly:?} + '/marker').read() == 'readonly'
for path in [{outside:?}, {credential:?}, {alias:?}]:
    try: open(path).read()
    except OSError: pass
    else: raise AssertionError('ungranted read: ' + path)
try: open({readonly:?} + '/marker', 'w').write('changed')
except OSError: pass
else: raise AssertionError('read-only write succeeded')
try: os.mkdir('/ungranted')
except OSError: pass
else: raise AssertionError('ungranted root write succeeded')
assert os.path.isfile('/proc/self/oom_score_adj')
try: open('/proc/self/oom_score_adj', 'w').write('1')
except OSError: pass
else: raise AssertionError('procfs write succeeded')
with tempfile.NamedTemporaryFile() as scratch:
    scratch.write(b'scratch-reached'); scratch.flush()
    assert os.path.commonpath([os.path.realpath(scratch.name), {root:?}]) == {root:?}
    assert open(scratch.name, 'rb').read() == b'scratch-reached'
for host in ['127.0.0.1', '::1']:
    family = socket.AF_INET6 if ':' in host else socket.AF_INET
    listener = socket.socket(family, socket.SOCK_STREAM)
    listener.bind((host, 0)); listener.listen(1)
    client = socket.socket(family, socket.SOCK_STREAM)
    client.connect(listener.getsockname())
    accepted, _ = listener.accept()
    accepted.close(); client.close(); listener.close()
remote = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
try: remote.sendto(b'no egress', ('198.51.100.1', 9))
except OSError as error: assert error.errno == errno.ENETUNREACH, error
else: raise AssertionError('off-host egress succeeded')
libc = ctypes.CDLL(None, use_errno=True)
assert libc.unshare(0) == -1 and ctypes.get_errno() == errno.EPERM
assert not os.path.exists('/proc/{host_pid}/cmdline')
open('marker', 'w').write('inside')
print('filesystem-loopback-egress-seccomp-pid-boundary-reached')
",
        readonly = readonly.display().to_string(),
        outside = outside.display().to_string(),
        credential = credential.display().to_string(),
        alias = alias.display().to_string(),
        root = root.display().to_string(),
        host_pid = std::process::id()
    );
    let source = format!(
        r#"
fn main(harness: Harness) {{
  const host = harness.system.sandbox_confinement()
  if !host.confines_processes {{
    harness.stdio.println("bubblewrap-unavailable")
    return
  }}
  assert_eq(host.mechanism, "linux_bubblewrap")
  const result = harness.tools.run_command({{argv: ["/usr/bin/python3", "-c", {probe}]}})
  assert_eq(result.exit_code, 0)
  assert_eq(result.sandbox.kind, "bubblewrap")
  assert_eq(trim(result.stdout), "filesystem-loopback-egress-seccomp-pid-boundary-reached")
  harness.stdio.println("canonical-bubblewrap-boundary-reached")
}}
"#,
        probe = serde_json::to_string(&probe).unwrap()
    );
    let mut command = harn_e2e_command();
    command.current_dir(&root).env("HOME", &home).args([
        "run",
        "--standalone",
        "--sandbox-allow-process-self-introspection",
        "--allow-process-loopback",
        "--read-only-root",
        &readonly.display().to_string(),
        "--sandbox-read-root",
        &home.display().to_string(),
        "-e",
        &source,
    ]);
    deny_landlock(&mut command, false);
    let output = command.output().unwrap();
    assert!(output.status.success(), "{output:?}");
    if output.stdout == b"bubblewrap-unavailable\n" {
        eprintln!("NOT EXERCISED: functional bubblewrap namespaces unavailable");
        assert_ne!(std::env::var("BWRAP_REQUIRE_TESTS").as_deref(), Ok("1"));
        return;
    }
    assert_eq!(output.stdout, b"canonical-bubblewrap-boundary-reached\n");
    assert_eq!(std::fs::read(root.join("marker")).unwrap(), b"inside");
    assert_eq!(std::fs::read(readonly.join("marker")).unwrap(), b"readonly");
    assert_eq!(std::fs::read(&outside).unwrap(), b"known-outside");
}

#[test]
fn canonical_fixture_scrubs_ambient_loader_controls_without_scrubbing_explicit_controls() {
    const CHILD: &str = "FIXTURE_LOADER_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "bubblewrap_fallback_e2e::canonical_fixture_scrubs_ambient_loader_controls_without_scrubbing_explicit_controls",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("MALLOC_ARENA_MAX", "2")
            .env("FIXTURE_LOADER_SENTINEL", "retained")
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(String::from_utf8_lossy(&output.stdout).contains("ambient-loader-fixture-reached"));
        return;
    }
    assert_eq!(std::env::var("MALLOC_ARENA_MAX").as_deref(), Ok("2"));
    let root = tempfile::tempdir().unwrap();
    let source = r#"
fn main(harness: Harness) {
  const host = harness.system.sandbox_confinement()
  assert_eq(host.mechanism, "linux_bubblewrap")
  assert_eq(host.confines_processes, true)
  const result = harness.tools.run_command({argv: ["/usr/bin/sh", "-c", "test -z \"$MALLOC_ARENA_MAX\" && test \"$FIXTURE_LOADER_SENTINEL\" = retained && printf ambient-loader-fixture-reached"]})
  assert_eq(result.exit_code, 0)
  assert_eq(result.stdout, "ambient-loader-fixture-reached")
  harness.stdio.println(result.stdout)
}
"#;
    let mut allowed = harn_e2e_command();
    allowed.current_dir(root.path()).args([
        "run",
        "--standalone",
        "--sandbox-allow-process-self-introspection",
        "-e",
        source,
    ]);
    deny_landlock(&mut allowed, false);
    let output = allowed.output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"ambient-loader-fixture-reached\n");

    let mut denied = harn_e2e_command();
    denied
        .current_dir(root.path())
        .env("MALLOC_ARENA_MAX", "2")
        .args([
            "run",
            "--standalone",
            "--sandbox-allow-process-self-introspection",
            "-e",
            source,
        ]);
    deny_landlock(&mut denied, false);
    let output = denied.output().unwrap();
    assert!(!output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("MALLOC_ARENA_MAX"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("category: ToolRejected"));
    assert!(
        output.stdout.is_empty(),
        "denied payload must not fire: {output:?}"
    );
    println!("ambient-loader-fixture-reached");
}

#[test]
fn canonical_trusted_setup_refuses_loader_controls_before_payload() {
    for (name, value) in [
        ("LD_TRACE_LOADED_OBJECTS", "1"),
        ("GLIBC_TUNABLES", "glibc.malloc.trim_threshold=131072"),
        ("MALLOC_TRIM_THRESHOLD_", "131072"),
    ] {
        trusted_setup_refuses_control(name, value);
    }
}

fn trusted_setup_refuses_control(name: &str, value: &str) {
    let root = tempfile::tempdir().unwrap();
    let source = r#"
fn main(harness: Harness) {
  const host = harness.system.sandbox_confinement()
  if !host.confines_processes {
    harness.stdio.println("bubblewrap-unavailable")
    return
  }
  assert_eq(host.mechanism, "linux_bubblewrap")
  for mode in ["patch", "replace"] {
    let refused = false
    try {
      const result = harness.tools.run_command({argv: ["/usr/bin/sh", "-c", "printf reached > marker"], env_mode: mode, env: {LD_TRACE_LOADED_OBJECTS: "1"}})
      harness.stdio.println(json_stringify(result))
    } catch refusal {
      assert_eq(refusal.kind, "backend_error")
      assert_eq(contains(refusal.message, "LD_TRACE_LOADED_OBJECTS"), true)
      refused = true
    }
    assert_eq(refused, true)
  }
  const allowed = harness.tools.run_command({argv: ["/usr/bin/sh", "-c", "test \"$ORDINARY_PROBE\" = admitted; printf ordinary"], env: {ORDINARY_PROBE: "admitted"}})
  assert_eq(allowed.exit_code, 0)
  assert_eq(allowed.stdout, "ordinary")
  for mode in ["merge", "replace"] {
    let refused = false
    try {
      harness.process.run({program: "/usr/bin/sh", args: ["-c", "printf reached > marker"], env_mode: mode, env: {LD_TRACE_LOADED_OBJECTS: "1"}})
    } catch refusal {
      assert_eq(contains(refusal.message, "LD_TRACE_LOADED_OBJECTS"), true)
      refused = true
    }
    assert_eq(refused, true)
  }
  const asynchronous = harness.process.run({program: "/usr/bin/sh", args: ["-c", "test \"$ORDINARY_PROBE\" = admitted; printf asynchronous"], env: {ORDINARY_PROBE: "admitted"}})
  assert_eq(asynchronous.exit_code, 0)
  assert_eq(asynchronous.stdout, "asynchronous")
  harness.stdio.println("loader-control-refused-ordinary-environment-reached")
}
"#
    .replace("LD_TRACE_LOADED_OBJECTS", name)
    .replace("\"1\"", &serde_json::to_string(value).unwrap());
    let mut command = harn_e2e_command();
    command.current_dir(root.path()).args([
        "run",
        "--standalone",
        "--sandbox-allow-process-self-introspection",
        "-e",
        &source,
    ]);
    deny_landlock(&mut command, false);
    let output = command.output().unwrap();
    assert!(output.status.success(), "{name}: {output:?}");
    if output.stdout == b"bubblewrap-unavailable\n" {
        eprintln!("NOT EXERCISED: functional bubblewrap namespaces unavailable");
        assert_ne!(std::env::var("BWRAP_REQUIRE_TESTS").as_deref(), Ok("1"));
        return;
    }
    assert_eq!(
        output.stdout,
        b"loader-control-refused-ordinary-environment-reached\n"
    );
    assert!(!root.path().join("marker").exists());
}

#[test]
fn canonical_trusted_setup_composes_inherited_removal_and_clear() {
    for (name, value) in [
        ("LD_BIND_NOW", "1"),
        ("GLIBC_TUNABLES", "glibc.malloc.trim_threshold=131072"),
        ("MALLOC_TRIM_THRESHOLD_", "131072"),
    ] {
        trusted_setup_composes_control(name, value);
    }
}

fn trusted_setup_composes_control(name: &str, value: &str) {
    let root = tempfile::tempdir().unwrap();
    let source = r#"
fn main(harness: Harness) {
  const host = harness.system.sandbox_confinement()
  if !host.confines_processes {
    harness.stdio.println("bubblewrap-unavailable")
    return
  }
  assert_eq(host.mechanism, "linux_bubblewrap")
  let refused = false
  try {
    harness.tools.run_command({argv: ["/usr/bin/sh", "-c", "printf reached > marker"], env_mode: "patch"})
  } catch refusal {
    assert_eq(refusal.kind, "backend_error")
    assert_eq(contains(refusal.message, "LD_BIND_NOW"), true)
    refused = true
  }
  assert_eq(refused, true)
  const removed = harness.tools.run_command({argv: ["/usr/bin/sh", "-c", "test -z \"$LD_BIND_NOW\"; printf removed"], env_mode: "patch", env_remove: ["LD_BIND_NOW"]})
  assert_eq(removed.exit_code, 0)
  assert_eq(removed.stdout, "removed")
  const cleared = harness.tools.run_command({argv: ["/usr/bin/sh", "-c", "test -z \"$LD_BIND_NOW\" && test \"$ORDINARY_PROBE\" = admitted; printf cleared"], env_mode: "replace", env: {ORDINARY_PROBE: "admitted"}})
  assert_eq(cleared.exit_code, 0)
  assert_eq(cleared.stdout, "cleared")
  let asynchronous_refused = false
  try {
    harness.process.run({program: "/usr/bin/sh", args: ["-c", "printf reached > marker"]})
  } catch refusal {
    assert_eq(contains(refusal.message, "LD_BIND_NOW"), true)
    asynchronous_refused = true
  }
  assert_eq(asynchronous_refused, true)
  const asynchronous_removed = harness.process.run({program: "/usr/bin/sh", args: ["-c", "test -z \"$LD_BIND_NOW\"; printf removed"], env_remove: ["LD_BIND_NOW"]})
  assert_eq(asynchronous_removed.exit_code, 0)
  assert_eq(asynchronous_removed.stdout, "removed")
  const asynchronous_cleared = harness.process.run({program: "/usr/bin/sh", args: ["-c", "test -z \"$LD_BIND_NOW\"; printf cleared"], env_mode: "replace"})
  assert_eq(asynchronous_cleared.exit_code, 0)
  assert_eq(asynchronous_cleared.stdout, "cleared")
  harness.stdio.println("inherited-refused-removal-and-clear-reached")
}
"#
    .replace("LD_BIND_NOW", name);
    let mut command = harn_e2e_command();
    command.current_dir(root.path()).env(name, value).args([
        "run",
        "--standalone",
        "--sandbox-allow-process-self-introspection",
        "-e",
        &source,
    ]);
    deny_landlock(&mut command, false);
    let output = command.output().unwrap();
    assert!(output.status.success(), "{name}: {output:?}");
    if output.stdout == b"bubblewrap-unavailable\n" {
        eprintln!("NOT EXERCISED: functional bubblewrap namespaces unavailable");
        assert_ne!(std::env::var("BWRAP_REQUIRE_TESTS").as_deref(), Ok("1"));
        return;
    }
    assert_eq!(
        output.stdout,
        b"inherited-refused-removal-and-clear-reached\n"
    );
    assert!(!root.path().join("marker").exists());
}

#[test]
fn canonical_direct_landlock_preserves_payload_loader_environment() {
    let root = tempfile::tempdir().unwrap();
    let source = r#"
fn main(harness: Harness) {
  const host = harness.system.sandbox_confinement()
  if host.mechanism != "linux_landlock" || !host.confines_processes {
    harness.stdio.println("landlock-unavailable")
    return
  }
  const synchronous = harness.tools.run_command({argv: ["/usr/bin/sh", "-c", "test \"$LD_BIND_NOW\" = 1 && test \"$GLIBC_TUNABLES\" = glibc.malloc.trim_threshold=131072 && test \"$MALLOC_TRIM_THRESHOLD_\" = 131072 && printf synchronous"], env: {LD_BIND_NOW: "1", GLIBC_TUNABLES: "glibc.malloc.trim_threshold=131072", MALLOC_TRIM_THRESHOLD_: "131072"}})
  assert_eq(synchronous.exit_code, 0)
  assert_eq(synchronous.stdout, "synchronous")
  const asynchronous = harness.process.run({program: "/usr/bin/sh", args: ["-c", "test \"$LD_BIND_NOW\" = 1 && test \"$GLIBC_TUNABLES\" = glibc.malloc.trim_threshold=131072 && test \"$MALLOC_TRIM_THRESHOLD_\" = 131072 && printf asynchronous"], env: {LD_BIND_NOW: "1", GLIBC_TUNABLES: "glibc.malloc.trim_threshold=131072", MALLOC_TRIM_THRESHOLD_: "131072"}})
  assert_eq(asynchronous.exit_code, 0)
  assert_eq(asynchronous.stdout, "asynchronous")
  const inherited_synchronous = harness.tools.run_command({argv: ["/usr/bin/sh", "-c", "test \"$LD_BIND_NOW\" = 1 && test \"$GLIBC_TUNABLES\" = glibc.malloc.trim_threshold=131072 && test \"$MALLOC_TRIM_THRESHOLD_\" = 131072 && printf inherited-synchronous"], env_mode: "patch"})
  assert_eq(inherited_synchronous.exit_code, 0)
  assert_eq(inherited_synchronous.stdout, "inherited-synchronous")
  const inherited_asynchronous = harness.process.exec("/usr/bin/sh", "-c", "test \"$LD_BIND_NOW\" = 1 && test \"$GLIBC_TUNABLES\" = glibc.malloc.trim_threshold=131072 && test \"$MALLOC_TRIM_THRESHOLD_\" = 131072 && printf inherited-asynchronous")
  assert_eq(inherited_asynchronous.exit_code, 0)
  assert_eq(inherited_asynchronous.stdout, "inherited-asynchronous")
  harness.stdio.println("direct-confinement-payload-environment-reached")
}
"#;
    let output = harn_e2e_command()
        .current_dir(root.path())
        .env("LD_BIND_NOW", "1")
        .env("GLIBC_TUNABLES", "glibc.malloc.trim_threshold=131072")
        .env("MALLOC_TRIM_THRESHOLD_", "131072")
        .args([
            "run",
            "--standalone",
            "--sandbox-allow-process-self-introspection",
            "-e",
            source,
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    if output.stdout == b"landlock-unavailable\n" {
        eprintln!("NOT EXERCISED: functional Landlock unavailable");
        assert_ne!(
            std::env::var("HARN_REQUIRE_LANDLOCK_TESTS").as_deref(),
            Ok("1")
        );
        return;
    }
    assert_eq!(
        output.stdout,
        b"direct-confinement-payload-environment-reached\n"
    );
}

#[test]
fn neither_kernel_boundary_refuses_with_structure_before_the_payload() {
    let root = tempfile::tempdir().unwrap();
    let source = r#"
fn main(harness: Harness) {
  try {
    harness.tools.run_command({argv: ["/usr/bin/sh", "-c", "printf escaped > marker"]})
    throw "payload was allowed"
  } catch refusal {
    assert_eq(refusal.source, "sandbox_mechanism")
    assert_eq(refusal.sandbox_mechanism.mechanism, "linux_bubblewrap")
    assert_eq(refusal.sandbox_mechanism.availability, "absent_on_host")
    assert_eq(refusal.sandbox_mechanism.unconfined, ["writes", "reads", "credential_reads", "network"])
  }
  harness.stdio.println("structured-unavailable-before-payload")
}
"#;
    let mut command = harn_e2e_command();
    command.current_dir(root.path()).args([
        "run",
        "--standalone",
        "--sandbox-allow-process-self-introspection",
        "-e",
        source,
    ]);
    deny_landlock(&mut command, true);
    let output = command.output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"structured-unavailable-before-payload\n");
    assert!(!root.path().join("marker").exists());
}

#[test]
fn canonical_owner_death_kills_the_private_pid_tree_including_a_detached_grandchild() {
    assert_private_pid_tree_ends(false);
}

#[test]
fn canonical_cancel_handle_kills_the_private_pid_tree_including_a_detached_grandchild() {
    assert_private_pid_tree_ends(true);
}

fn assert_private_pid_tree_ends(cancel: bool) {
    use super::guardian_namespace_e2e::{await_exit, pidfd, Owner};
    use std::io::{BufRead, BufReader};
    use std::os::fd::AsRawFd;
    use std::process::Stdio;

    let root = tempfile::tempdir().unwrap();
    let mut preflight = harn_e2e_command();
    preflight.current_dir(root.path()).args([
        "run", "--standalone", "-e",
        "fn main(harness: Harness) { harness.stdio.println(harness.system.sandbox_confinement().confines_processes) }",
    ]);
    deny_landlock(&mut preflight, false);
    let preflight = preflight.output().unwrap();
    assert!(preflight.status.success(), "{preflight:?}");
    if preflight.stdout == b"false\n" {
        eprintln!("NOT EXERCISED: functional bubblewrap namespaces unavailable");
        assert_ne!(std::env::var("BWRAP_REQUIRE_TESTS").as_deref(), Ok("1"));
        return;
    }
    assert_eq!(preflight.stdout, b"true\n");
    let fifo = root.path().join("ready.fifo");
    let fifo_c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
    let report = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&fifo)
        .unwrap();
    let stop = root.path().join("stop.fifo");
    let stop_c = std::ffi::CString::new(stop.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(stop_c.as_ptr(), 0o600) }, 0);
    let probe = r#"
import signal, subprocess, sys
grandchild = subprocess.Popen([sys.executable, '-c', "import signal; print('ready', flush=True); signal.pause()"], start_new_session=True, stdout=subprocess.PIPE)
assert grandchild.stdout.readline() == b'ready\n'
with open(sys.argv[1], 'w') as report:
    report.write('private-pid-tree-ready\n'); report.flush()
signal.pause()
"#;
    let terminal = if cancel {
        format!(
            r#"
  const release = harness.tools.run_command({{argv: ["/usr/bin/sh", "-c", "read token < \"$1\"; test \"$token\" = cancel", "probe", {stop}]}})
  assert_eq(release.exit_code, 0)
  const cancelled = harness.tools.cancel_handle({{handle_id: child.handle_id ?? "", wait_result_ms: 60000}})
  assert_eq(cancelled.cancelled, true)
  assert_eq(cancelled.result.status, "killed")
  harness.stdio.println("canonical-cancel-tree-exited")
"#,
            stop = serde_json::to_string(&stop.display().to_string()).unwrap()
        )
    } else {
        "const result = harness.tools.wait_command({handle_id: child.handle_id ?? \"\", timeout_ms: 60000})\nthrow json_stringify(result)".into()
    };
    let source = format!(
        r#"
fn main(harness: Harness) {{
  const child = harness.tools.run_command({{argv: ["/usr/bin/python3", "-c", {probe}, {fifo}], background: true}})
  assert_eq(child.sandbox.kind, "bubblewrap")
  harness.stdio.println(json_stringify(child))
  {terminal}
}}
"#,
        probe = serde_json::to_string(probe).unwrap(),
        fifo = serde_json::to_string(&fifo.display().to_string()).unwrap()
    );
    let mut command = harn_e2e_command();
    command
        .current_dir(root.path())
        .args([
            "run",
            "--standalone",
            "--sandbox-allow-process-self-introspection",
            "-e",
            &source,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .process_group(0);
    deny_landlock(&mut command, false);
    let mut owner = Owner(command.spawn().unwrap());
    let mut readiness = libc::pollfd {
        fd: report.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    assert_eq!(
        unsafe { libc::poll(&raw mut readiness, 1, 30_000) },
        1,
        "confined payload and detached grandchild never reported readiness"
    );
    let mut line = String::new();
    BufReader::new(report).read_line(&mut line).unwrap();
    assert_eq!(line, "private-pid-tree-ready\n");
    line.clear();
    let mut output = BufReader::new(owner.0.stdout.take().unwrap());
    output.read_line(&mut line).unwrap();
    let receipt: serde_json::Value = serde_json::from_str(&line).unwrap();
    let worker = receipt["pid"].as_i64().filter(|pid| *pid > 0).unwrap() as i32;
    let group = receipt["process_group_id"].as_i64().unwrap() as i32;
    let mut processes = vec![worker];
    let mut cursor = 0;
    while cursor < processes.len() {
        let pid = processes[cursor];
        let children = std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children")).unwrap();
        processes.extend(
            children
                .split_whitespace()
                .map(|pid| pid.parse::<i32>().unwrap()),
        );
        cursor += 1;
    }
    assert!(
        processes.len() >= 4,
        "wrapper, namespace init, payload and grandchild must be live: {processes:?}"
    );
    assert!(
        processes.iter().any(|pid| {
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
            stat.rsplit_once(") ")
                .unwrap()
                .1
                .split_whitespace()
                .nth(2)
                .unwrap()
                .parse::<i32>()
                .unwrap()
                != group
        }),
        "the actual grandchild must escape the worker group before owner death"
    );
    let live = processes.into_iter().map(pidfd).collect::<Vec<_>>();
    for process in &live {
        let mut event = libc::pollfd {
            fd: process.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        assert_eq!(unsafe { libc::poll(&raw mut event, 1, 0) }, 0);
    }
    if cancel {
        std::fs::write(&stop, b"cancel\n").unwrap();
        assert!(owner.0.wait().unwrap().success());
        let mut terminal = String::new();
        std::io::Read::read_to_string(&mut output, &mut terminal).unwrap();
        assert_eq!(terminal, "canonical-cancel-tree-exited\n");
    } else {
        assert_eq!(
            unsafe { libc::kill(-(owner.0.id() as i32), libc::SIGKILL) },
            0
        );
        owner.0.wait().unwrap();
    }
    for process in live {
        await_exit(&process);
    }
}
