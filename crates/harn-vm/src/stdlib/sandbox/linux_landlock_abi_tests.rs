//! Landlock ABI negotiation: what each ABI is held to, and what an older one
//! costs. The live cases cap the negotiated ABI below the kernel's. An
//! unhandled right behaves the same on every kernel, so a capped ruleset is
//! the ruleset an older kernel would build.

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;

use super::*;
use crate::stdlib::sandbox::enforcement::MechanismLimit;
use ::landlock::Access;

const EVERY_ABI: [ABI; 6] = [ABI::V1, ABI::V2, ABI::V3, ABI::V4, ABI::V5, ABI::V6];

#[test]
fn limits_name_each_gap_an_older_abi_leaves() {
    use MechanismLimit::*;
    let limits = |abi| LandlockSupport::assume(abi).limits();
    assert_eq!(
        limits(ABI::V1),
        vec![
            CrossDirectoryRenameRefused,
            TruncationFilteredBySyscall,
            DeviceIoctlUnscoped,
            UnixSocketGrantUnavailable
        ]
    );
    assert_eq!(
        limits(ABI::V2),
        vec![
            TruncationFilteredBySyscall,
            DeviceIoctlUnscoped,
            UnixSocketGrantUnavailable
        ]
    );
    assert_eq!(
        limits(ABI::V4),
        vec![DeviceIoctlUnscoped, UnixSocketGrantUnavailable]
    );
    assert_eq!(limits(ABI::V5), vec![UnixSocketGrantUnavailable]);
    assert_eq!(limits(ABI::V6), Vec::new());
}

/// Negotiation picks the newest ABI the kernel accepts, never one it refuses.
#[test]
fn negotiation_selects_the_newest_abi_the_kernel_accepts() {
    let kernel = |newest: Option<ABI>| move |abi: ABI| newest.is_some_and(|newest| abi <= newest);
    let selected = |newest, ceiling| {
        LandlockSupport::negotiate_with(ceiling, kernel(newest)).map(LandlockSupport::abi)
    };
    assert_eq!(selected(Some(ABI::V1), ABI::V6), Some(ABI::V1));
    assert_eq!(selected(Some(ABI::V5), ABI::V6), Some(ABI::V5));
    assert_eq!(selected(Some(ABI::V6), ABI::V6), Some(ABI::V6));
    assert_eq!(selected(Some(ABI::V9), ABI::V6), Some(ABI::V6));
    assert_eq!(selected(None, ABI::V6), None, "no Landlock selects nothing");
    // The ceiling caps a newer kernel without inventing support.
    assert_eq!(selected(Some(ABI::V6), ABI::V2), Some(ABI::V2));
    // Negative control: a kernel that refuses ABI 6 is never handed it, even
    // when it would accept a weaker feature asked for out of order.
    let gappy = |abi: ABI| abi != ABI::V6 && abi <= ABI::V5;
    assert_eq!(
        LandlockSupport::negotiate_with(ABI::V6, gappy).map(LandlockSupport::abi),
        Some(ABI::V5)
    );
}

/// The live kernel's answer: every ABI at or below the negotiated one creates
/// a ruleset, and the next one up, if Harn distinguishes it, does not.
#[test]
fn the_negotiated_abi_is_the_one_the_live_kernel_creates() {
    if !live_landlock_available("landlock-abi-negotiation") {
        return;
    }
    let support = LandlockSupport::negotiate(ABI::V6).expect("Landlock is available");
    for abi in EVERY_ABI {
        assert_eq!(
            capped(abi).is_some(),
            abi <= support.abi(),
            "ABI {abi} against negotiated ABI {}",
            support.abi()
        );
    }
}

#[test]
fn handled_rights_follow_the_abi_and_stop_at_harns_vocabulary() {
    let handled = |abi| LandlockSupport::assume(abi).handled_access();
    // Every ABI bounds reads, writes, and execution.
    for abi in EVERY_ABI {
        assert!(
            handled(abi).contains(AccessFs::from_all(ABI::V1)),
            "ABI {abi} must handle the whole ABI 1 vocabulary"
        );
    }
    assert!(!handled(ABI::V1).contains(AccessFs::Refer));
    assert!(handled(ABI::V2).contains(AccessFs::Refer));
    assert!(!handled(ABI::V2).contains(AccessFs::Truncate));
    assert!(handled(ABI::V3).contains(AccessFs::Truncate));
    assert!(!handled(ABI::V4).contains(AccessFs::IoctlDev));
    assert!(handled(ABI::V5).contains(AccessFs::IoctlDev));
    // Negative control: a newer kernel never adds a right Harn's grants do
    // not use. Handling `RESOLVE_UNIX` would refuse every socket connect.
    assert_eq!(handled(ABI::V6), AccessFs::from_all(ABI::V5));
    assert!(!handled(ABI::V6).contains(AccessFs::ResolveUnix));
}

#[test]
fn the_unix_socket_grant_alone_demands_abi_6() {
    let workspace = tempfile::tempdir().expect("workspace");
    let mut policy = abi_policy(workspace.path());
    policy.process_sandbox.unix_socket_roots = vec![workspace.path().display().to_string()];
    let refusal = landlock_profile_for("/bin/true", &policy, LandlockSupport::assume(ABI::V5))
        .err()
        .expect("a serve-only socket grant must refuse below ABI 6");
    assert!(
        refusal.to_string().contains("ABI 6") && refusal.to_string().contains("ABI 5"),
        "the refusal names both ABIs: {refusal}"
    );
    // Negative control: the same ABI renders a policy without the grant.
    if let Some(support) = capped(ABI::V5) {
        landlock_profile_for("/bin/true", &abi_policy(workspace.path()), support)
            .expect("ABI 5 renders the default grants");
    }
}

/// The filter is compiled differently only when Landlock cannot bound
/// truncation, so a host at ABI 3 or newer keeps its exact program.
#[test]
fn only_an_abi_without_truncation_control_changes_the_syscall_filter() {
    let policy = abi_policy(Path::new("/ws"));
    let bounded = compile_seccomp_program(&policy, true).expect("bounded filter");
    let filtered = compile_seccomp_program(&policy, false).expect("filtered filter");
    assert_ne!(bounded, filtered);
    let mut rules: std::collections::BTreeMap<libc::c_long, Vec<SeccompRule>> =
        allowed_syscalls(&policy)
            .into_iter()
            .map(|syscall| (syscall, Vec::new()))
            .collect();
    landlock_support::filter_path_truncation(&mut rules).expect("truncation rules");
    assert!(!rules.contains_key(&libc::SYS_truncate));
    assert!(!rules.contains_key(&libc::SYS_openat2));
    assert_eq!(rules[&libc::SYS_openat].len(), 3);
}

/// Every negotiated ABI holds the filesystem boundary: inside writes land,
/// outside writes and reads are refused.
#[test]
fn every_negotiated_abi_confines_reads_and_writes_to_the_grants() {
    if !live_landlock_available("landlock-abi-boundary") {
        return;
    }
    let workspace = tempfile::tempdir().expect("workspace");
    let outside = tempfile::tempdir().expect("outside");
    std::fs::write(outside.path().join("secret"), "outside").expect("seed secret");
    let policy = abi_policy(workspace.path());
    let inside = c_path(&workspace.path().join("inside"));
    let escaped = c_path(&outside.path().join("escaped"));
    let secret = c_path(&outside.path().join("secret"));
    for abi in EVERY_ABI {
        let Some(support) = capped(abi) else { continue };
        let profile = profile_setup_for("/bin/true", &policy, support).expect("profile");
        let created = |path: &CString| {
            let fd = unsafe { libc::open(path.as_ptr(), libc::O_WRONLY | libc::O_CREAT, 0o600) };
            fd >= 0
        };
        let inside_written = run_confined(&profile, || created(&inside));
        let outside_written = run_confined(&profile, || created(&escaped));
        let outside_read = run_confined(&profile, || unsafe {
            libc::open(secret.as_ptr(), libc::O_RDONLY) >= 0
        });
        assert!(inside_written, "ABI {abi}: a workspace write must land");
        assert!(
            !outside_written,
            "ABI {abi}: an outside write must be refused"
        );
        assert!(!outside_read, "ABI {abi}: an outside read must be refused");
        assert!(!outside.path().join("escaped").exists());
    }
}

/// Below ABI 3 Landlock lets a child truncate any file its uid owns by path.
/// The syscall filter is what closes that: without it the outside file is
/// emptied, with it the file is untouched.
#[test]
fn the_syscall_filter_closes_truncation_below_abi_3() {
    if !live_landlock_available("landlock-abi-truncation") {
        return;
    }
    let workspace = tempfile::tempdir().expect("workspace");
    let outside = tempfile::tempdir().expect("outside");
    let target = outside.path().join("victim");
    let policy = abi_policy(workspace.path());
    let path = c_path(&target);
    let truncate_by_path = || unsafe { libc::truncate(path.as_ptr(), 0) == 0 };
    let truncate_on_read_open =
        || unsafe { libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_TRUNC) >= 0 };
    for abi in [ABI::V1, ABI::V2, ABI::V3] {
        let Some(support) = capped(abi) else { continue };
        let profile = profile_setup_for("/bin/true", &policy, support).expect("profile");
        for (name, attempt) in [
            ("truncate(2)", &truncate_by_path as &dyn Fn() -> bool),
            ("O_RDONLY|O_TRUNC", &truncate_on_read_open),
        ] {
            std::fs::write(&target, "intact").expect("seed");
            assert!(
                !run_confined(&profile, attempt),
                "ABI {abi}: {name} succeeded"
            );
            assert_eq!(
                std::fs::read(&target).unwrap(),
                b"intact",
                "ABI {abi}: {name}"
            );
        }
    }
    // Negative control: the same ABI 2 ruleset without the filter leaves the
    // gap open, so the assertions above measure the filter, not an accident.
    let Some(support) = capped(ABI::V2) else {
        return;
    };
    let mut unfiltered = profile_setup_for("/bin/true", &policy, support).expect("profile");
    unfiltered.seccomp = compile_seccomp_program(&policy, true).expect("unfiltered program");
    std::fs::write(&target, "intact").expect("seed");
    assert!(run_confined(&unfiltered, truncate_by_path));
    assert_eq!(std::fs::read(&target).unwrap(), b"");
}

/// ABI 1 refuses a rename into another directory even inside the workspace;
/// ABI 2 permits it. Reported as `cross_directory_rename_refused`.
#[test]
fn cross_directory_rename_is_refused_only_below_abi_2() {
    if !live_landlock_available("landlock-abi-refer") {
        return;
    }
    let workspace = tempfile::tempdir().expect("workspace");
    std::fs::create_dir(workspace.path().join("a")).unwrap();
    std::fs::create_dir(workspace.path().join("b")).unwrap();
    let policy = abi_policy(workspace.path());
    let from = c_path(&workspace.path().join("a/file"));
    let to = c_path(&workspace.path().join("b/file"));
    let rename = || unsafe { libc::rename(from.as_ptr(), to.as_ptr()) == 0 };
    for (abi, expected) in [(ABI::V1, false), (ABI::V2, true)] {
        let Some(support) = capped(abi) else { continue };
        std::fs::write(workspace.path().join("a/file"), "moved").unwrap();
        let _ = std::fs::remove_file(workspace.path().join("b/file"));
        let profile = profile_setup_for("/bin/true", &policy, support).expect("profile");
        assert_eq!(run_confined(&profile, rename), expected, "ABI {abi}");
        assert_eq!(
            support
                .limits()
                .contains(&MechanismLimit::CrossDirectoryRenameRefused),
            !expected,
            "ABI {abi}: the limit is reported exactly when the kernel refuses"
        );
    }
}

/// The kernel's support when it reaches `abi`; `None` on an older kernel, so a
/// case never runs one ABI under another's name.
fn capped(abi: ABI) -> Option<LandlockSupport> {
    LandlockSupport::negotiate(abi).filter(|support| support.abi() == abi)
}

fn abi_policy(workspace: &Path) -> CapabilityPolicy {
    let mut policy = linux_policy_with_workspace_ops(&["read_text", "write_text", "delete"]);
    policy.workspace_roots = vec![workspace.display().to_string()];
    policy.side_effect_level = Some("process_exec".to_string());
    *policy.process_sandbox = crate::orchestration::ProcessSandboxPolicy {
        presets: Some(Vec::new()),
        ..Default::default()
    };
    policy
}

fn c_path(path: &Path) -> CString {
    CString::new(path.as_os_str().as_bytes()).expect("path without NUL")
}

/// Run `attempt` in a forked child after entering `profile`, returning what it
/// reported. The child makes only raw syscalls and exits without unwinding.
fn run_confined(profile: &ProcessProfile, attempt: impl Fn() -> bool) -> bool {
    let child = unsafe { libc::fork() };
    assert!(child >= 0, "fork failed");
    if child == 0 {
        let code = match apply_profile(profile) {
            Ok(()) => i32::from(!attempt()),
            Err(_) => 2,
        };
        unsafe { libc::_exit(code) };
    }
    let mut status = 0;
    assert_eq!(unsafe { libc::waitpid(child, &raw mut status, 0) }, child);
    assert!(libc::WIFEXITED(status), "child did not exit: {status}");
    match libc::WEXITSTATUS(status) {
        0 => true,
        1 => false,
        code => panic!("the child could not enter the profile (exit {code})"),
    }
}
