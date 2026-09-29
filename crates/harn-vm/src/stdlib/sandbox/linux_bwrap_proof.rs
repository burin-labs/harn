//! Test-only proof of a typed Linux fallback candidate and bubblewrap's real
//! filesystem boundary. This does not alter the production backend selector:
//! policy parity with Landlock and seccomp is still unproven.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use super::super::landlock_abi_version;

const REQUIRE_BWRAP_ENV: &str = "HARN_REQUIRE_BWRAP_TESTS";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CandidateBackend {
    Landlock,
    Bubblewrap,
    Refuse,
}

#[derive(Clone, Debug)]
struct LinuxBackendProbe {
    landlock_abi: u32,
    bwrap: Option<PathBuf>,
    bwrap_user_mount_probe: bool,
}

fn select_candidate(probe: &LinuxBackendProbe) -> CandidateBackend {
    if probe.landlock_abi > 0 {
        CandidateBackend::Landlock
    } else if probe.bwrap.is_some() && probe.bwrap_user_mount_probe {
        CandidateBackend::Bubblewrap
    } else {
        CandidateBackend::Refuse
    }
}

#[derive(Debug)]
struct MountPlan {
    read_only: Vec<(PathBuf, PathBuf)>,
    writable: Vec<(PathBuf, PathBuf)>,
}

fn render_bwrap(plan: &MountPlan, bwrap: &Path, program: &str, args: &[&str]) -> Command {
    let mut command = Command::new(bwrap);
    command.args([
        "--die-with-parent",
        "--new-session",
        "--unshare-user",
        "--unshare-pid",
    ]);
    for (source, destination) in &plan.read_only {
        command.args([
            "--ro-bind",
            source.to_str().expect("UTF-8 test path"),
            destination.to_str().expect("UTF-8 mount path"),
        ]);
    }
    for (source, destination) in &plan.writable {
        command.args([
            "--bind",
            source.to_str().expect("UTF-8 test path"),
            destination.to_str().expect("UTF-8 mount path"),
        ]);
    }
    command.args([
        "--proc", "/proc", "--dev", "/dev", "--tmpfs", "/tmp", "--", program,
    ]);
    command.args(args);
    command
}

fn bwrap_path() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|directory| directory.join("bwrap"))
        .find(|candidate| candidate.is_file())
}

fn bwrap_functional_probe(bwrap: &Path) -> bool {
    Command::new(bwrap)
        .args([
            "--unshare-user",
            "--unshare-pid",
            "--ro-bind",
            "/",
            "/",
            "--tmpfs",
            "/tmp",
            "--",
            "/usr/bin/true",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn run(command: &mut Command) -> Output {
    command
        .stdin(Stdio::null())
        .output()
        .expect("bubblewrap process should start")
}

fn bwrap_is_required() -> bool {
    std::env::var(REQUIRE_BWRAP_ENV)
        .map(|value| {
            let value = value.trim().to_ascii_lowercase();
            !matches!(value.as_str(), "" | "0" | "false" | "off" | "no")
        })
        .unwrap_or(false)
}

fn status_reason(bwrap: Option<&Path>, functional: bool) -> String {
    match (bwrap, functional) {
        (None, _) => "bubblewrap executable unavailable".to_string(),
        (Some(_), false) => "bubblewrap user and mount namespace probe failed".to_string(),
        (Some(path), true) => format!("bubblewrap probe passed at {}", path.display()),
    }
}

#[test]
fn typed_fallback_candidate_and_bwrap_mount_controls() {
    let bwrap = bwrap_path();
    let bwrap_works = bwrap.as_deref().is_some_and(bwrap_functional_probe);
    let probe = LinuxBackendProbe {
        landlock_abi: landlock_abi_version(),
        bwrap: bwrap.clone(),
        bwrap_user_mount_probe: bwrap_works,
    };
    let actual_selection = select_candidate(&probe);
    if !bwrap_works {
        let reason = status_reason(bwrap.as_deref(), bwrap_works);
        eprintln!(
            "[linux-bwrap-proof] backend_available=false exercised=0 selection={actual_selection:?}; {reason}"
        );
        assert!(
            !bwrap_is_required(),
            "{REQUIRE_BWRAP_ENV} requires a live bwrap proof: {reason}"
        );
        return;
    }

    // Force the capability input to the Landlock-absent case without changing
    // the host kernel or Harn's production selector. The result must be a
    // typed bwrap candidate only after the real user+mount namespace probe.
    let unsupported_landlock = LinuxBackendProbe {
        landlock_abi: 0,
        bwrap: bwrap.clone(),
        bwrap_user_mount_probe: true,
    };
    assert_eq!(
        select_candidate(&unsupported_landlock),
        CandidateBackend::Bubblewrap
    );
    assert_eq!(
        select_candidate(&LinuxBackendProbe {
            bwrap_user_mount_probe: false,
            ..unsupported_landlock.clone()
        }),
        CandidateBackend::Refuse
    );
    assert_eq!(
        select_candidate(&LinuxBackendProbe {
            bwrap: None,
            ..unsupported_landlock
        }),
        CandidateBackend::Refuse
    );

    let host = tempfile::tempdir().expect("temporary host tree");
    let outside_marker = host.path().join("outside-marker");
    std::fs::write(&outside_marker, b"outside").expect("create outside marker");
    let read_root = host.path().join("readonly");
    std::fs::create_dir(&read_root).expect("create read-only root");
    let read_marker = read_root.join("marker");
    std::fs::write(&read_marker, b"read-only").expect("create read-only marker");
    let write_root = host.path().join("workspace");
    std::fs::create_dir(&write_root).expect("create workspace root");

    let system_roots = ["/usr", "/etc"]
        .into_iter()
        .map(PathBuf::from)
        .filter(|path| path.exists())
        .map(|path| {
            let mount = path.clone();
            (path, mount)
        });
    let plan = MountPlan {
        read_only: system_roots
            .chain([(read_root.clone(), PathBuf::from("/readonly"))])
            .collect(),
        writable: vec![(write_root.clone(), PathBuf::from("/workspace"))],
    };
    let script = concat!(
        "test ! -e \"$1\" || exit 31; ",
        "test \"$(cat /readonly/marker)\" = read-only || exit 32; ",
        "if printf escaped > /readonly/marker 2>/dev/null; then exit 33; fi; ",
        "printf inside > /workspace/marker; test -s /workspace/marker; ",
        "printf payload-ran"
    );
    let result = run(&mut render_bwrap(
        &plan,
        bwrap.as_deref().expect("functional bwrap path"),
        "/usr/bin/sh",
        &[
            "-c",
            script,
            "probe",
            outside_marker.to_str().expect("UTF-8 test path"),
        ],
    ));
    assert!(
        result.status.success(),
        "bwrap boundary child failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(result.stdout, b"payload-ran");
    assert_eq!(
        std::fs::read(&write_root.join("marker")).unwrap(),
        b"inside"
    );
    assert_eq!(std::fs::read(&read_marker).unwrap(), b"read-only");
    assert_eq!(std::fs::read(&outside_marker).unwrap(), b"outside");

    let invalid_source = host.path().join("missing-source");
    let mut broken_plan = plan;
    broken_plan
        .writable
        .push((invalid_source, PathBuf::from("/missing-workspace")));
    let failed_setup = run(&mut render_bwrap(
        &broken_plan,
        bwrap.as_deref().expect("functional bwrap path"),
        "/usr/bin/sh",
        &[
            "-c",
            "printf payload-ran; printf touched > /workspace/setup-failure-marker",
        ],
    ));
    assert!(
        !failed_setup.status.success(),
        "invalid mount plan unexpectedly succeeded"
    );
    assert!(!failed_setup
        .stdout
        .windows(b"payload-ran".len())
        .any(|window| window == b"payload-ran"));
    assert!(
        !write_root.join("setup-failure-marker").exists(),
        "payload ran after wrapper setup failed"
    );

    eprintln!(
        "[linux-bwrap-proof] backend_available=true exercised=1 actual_selection={actual_selection:?} forced_landlock_absent={:?}; {}; controls=hidden-outside,read-only-bind,writable-bind,setup-failure-before-payload",
        select_candidate(&unsupported_landlock),
        status_reason(bwrap.as_deref(), bwrap_works),
    );
}
