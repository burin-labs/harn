//! Live proof of the direct Landlock `/dev/shm` grant, through the product's
//! own confined spawn: semaphores and process pools work, and nothing beyond
//! regular files in `/dev/shm` is granted.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use super::super::Backend;
use crate::orchestration::{CapabilityPolicy, SandboxProfile};
use crate::stdlib::sandbox::{SandboxBackend, SandboxMechanism};

pub(crate) const PYTHON: &str = "/usr/bin/python3";

/// A pool built on `fork`: the claim is the semaphore, not the start method
/// (3.14's default forkserver also needs a Unix socket).
pub(crate) const LOCK_AND_POOL: &str = "\
import concurrent.futures as cf, multiprocessing as m
l = m.Lock(); l.acquire(); l.release()
with cf.ProcessPoolExecutor(2, mp_context=m.get_context('fork')) as pool:
    assert list(pool.map(abs, [-1, -2])) == [1, 2]
print('pool:ok')
";

const PROBES: &str = "
import os, subprocess, sys
def probe(name, action):
    try:
        action()
        print(name + ':allowed')
    except OSError as error:
        print(f'{name}:{type(error).__name__}:{error.errno}')
tag, executable, outside = sys.argv[1:4]
def regular():
    with open(f'/dev/shm/{tag}-file', 'w') as handle:
        handle.write('x')
    os.unlink(f'/dev/shm/{tag}-file')
probe('regular', regular)
probe('mkdir', lambda: os.mkdir(f'/dev/shm/{tag}-dir'))
probe('symlink', lambda: os.symlink('/etc/hostname', f'/dev/shm/{tag}-link'))
probe('exec', lambda: subprocess.run([executable], check=True))
probe('dev_write', lambda: open(f'/dev/{tag}-file', 'w').close())
probe('outside_write', lambda: open(outside + '/escaped', 'w').close())
";

#[test]
fn a_direct_landlock_child_gets_semaphores_and_only_regular_files_in_dev_shm() {
    if !super::super::tests::live_landlock_available("shm-landlock") {
        return;
    }
    if !Path::new(PYTHON).is_file() || !Path::new(super::SHARED_MEMORY_DIR).is_dir() {
        eprintln!("[shm-landlock] exercised=0: {PYTHON} or /dev/shm is absent");
        return;
    }
    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
    let tag = format!("harn-shm-test-{}", std::process::id());
    let executable = Path::new(super::SHARED_MEMORY_DIR).join(format!("{tag}-exec"));
    std::fs::write(&executable, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    // Control: /dev/shm is not mounted noexec here, so a refused exec below is
    // Landlock's answer.
    assert!(std::process::Command::new(&executable)
        .status()
        .unwrap()
        .success());

    let policy = CapabilityPolicy {
        workspace_roots: vec![workspace.path().display().to_string()],
        sandbox_profile: SandboxProfile::Worktree,
        ..CapabilityPolicy::default()
    };
    assert_eq!(
        Backend::filesystem_mechanism(),
        SandboxMechanism::LinuxLandlock
    );
    assert!(super::super::resolve_netns_launcher(&policy)
        .unwrap()
        .is_none());
    eprintln!("[shm-landlock] mechanism=LinuxLandlock, no namespace helper");
    crate::orchestration::push_execution_policy(policy);
    let run = |code: String, args: &[String]| {
        let mut argv = vec!["-I".to_string(), "-c".to_string(), code];
        argv.extend_from_slice(args);
        crate::stdlib::sandbox::command_output(
            PYTHON,
            &argv,
            &crate::stdlib::sandbox::ProcessCommandConfig {
                cwd: Some(workspace.path().to_path_buf()),
                ..Default::default()
            },
        )
    };
    let pool = run(LOCK_AND_POOL.to_string(), &[]);
    let probes = run(
        PROBES.to_string(),
        &[
            tag,
            executable.display().to_string(),
            outside.path().display().to_string(),
        ],
    );
    crate::orchestration::pop_execution_policy();
    std::fs::remove_file(&executable).unwrap();

    let pool = pool.unwrap();
    assert!(
        pool.status.success(),
        "multiprocessing.Lock and a process pool must work confined\nstderr:\n{}",
        String::from_utf8_lossy(&pool.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&pool.stdout), "pool:ok\n");
    let probes = probes.unwrap();
    assert!(probes.status.success(), "{probes:?}");
    let denied = "PermissionError:13";
    assert_eq!(
        String::from_utf8_lossy(&probes.stdout),
        format!(
            "regular:allowed\nmkdir:{denied}\nsymlink:{denied}\nexec:{denied}\n\
             dev_write:{denied}\noutside_write:{denied}\n"
        ),
        "stderr:\n{}",
        String::from_utf8_lossy(&probes.stderr)
    );
    assert!(!outside.path().join("escaped").exists());
    eprintln!("[shm-landlock] lock+pool ok; mkdir, symlink, exec, /dev and outside writes refused");
}
