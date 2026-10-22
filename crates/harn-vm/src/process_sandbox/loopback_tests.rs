//! The rule is two-sided: a host without the launcher loses only the loopback
//! grant, and a host with one keeps it. Each side is the negative control of
//! the other, so neither can pass by always dropping or always keeping.

use super::*;
use crate::orchestration::SandboxProfile;

fn confined_policy_granting_loopback() -> CapabilityPolicy {
    let mut policy = CapabilityPolicy {
        workspace_roots: vec!["/ws".to_string()],
        sandbox_profile: SandboxProfile::Worktree,
        ..CapabilityPolicy::default()
    };
    policy.process_sandbox.allow_tcp_loopback = true;
    policy.process_sandbox.read_roots = vec!["/usr".to_string()];
    policy
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use crate::events::{add_event_sink, reset_event_sinks, CollectorSink};
    use std::rc::Rc;

    fn launcher_file() -> tempfile::NamedTempFile {
        use std::os::unix::fs::PermissionsExt;
        let file = tempfile::NamedTempFile::new().expect("create a stand-in launcher file");
        std::fs::set_permissions(file.path(), std::fs::Permissions::from_mode(0o755))
            .expect("make the stand-in executable");
        file
    }

    #[test]
    fn a_launcher_that_is_not_executable_is_unavailable() {
        use std::os::unix::fs::PermissionsExt;
        let file = tempfile::NamedTempFile::new().expect("file");
        std::fs::set_permissions(file.path(), std::fs::Permissions::from_mode(0o644)).unwrap();
        let mut policy = confined_policy_granting_loopback();
        policy.process_sandbox.netns_launcher_path = Some(file.path().display().to_string());
        let dropped = withhold_unsupported_loopback(&mut policy.process_sandbox).expect("withheld");
        assert_eq!(dropped.reason, LoopbackUnavailableReason::LauncherMissing);
    }

    #[test]
    fn a_host_without_a_launcher_loses_only_the_loopback_grant() {
        let mut policy = confined_policy_granting_loopback();
        let before = policy.clone();

        let dropped = withhold_unsupported_loopback(&mut policy.process_sandbox)
            .expect("no launcher means the grant must be withheld");

        assert_eq!(
            dropped.reason,
            LoopbackUnavailableReason::NoLauncherConfigured
        );
        assert!(!policy.process_sandbox.allow_tcp_loopback);
        let mut expected = before;
        expected.process_sandbox.allow_tcp_loopback = false;
        assert_eq!(policy, expected, "nothing but the one grant may change");
        let notice = dropped.notice();
        assert!(notice.contains("unavailable on this host"), "{notice}");
        assert!(notice.contains(NETNS_LAUNCHER_FIELD), "{notice}");
    }

    #[test]
    fn a_missing_or_relative_launcher_is_unavailable_and_named() {
        for (path, reason) in [
            (
                "/definitely/not/installed/launch",
                LoopbackUnavailableReason::LauncherMissing,
            ),
            (
                "relative/launch",
                LoopbackUnavailableReason::LauncherPathNotAbsolute,
            ),
        ] {
            let mut policy = confined_policy_granting_loopback();
            policy.process_sandbox.netns_launcher_path = Some(path.to_string());
            let dropped =
                withhold_unsupported_loopback(&mut policy.process_sandbox).expect("withheld");
            assert_eq!(dropped.reason, reason);
            assert!(dropped.notice().contains(path), "{}", dropped.notice());
        }
    }

    /// NEGATIVE CONTROL: with a launcher the grant is kept untouched.
    #[test]
    fn a_host_with_the_launcher_keeps_the_grant() {
        let launcher = launcher_file();
        let mut policy = confined_policy_granting_loopback();
        policy.process_sandbox.netns_launcher_path = Some(launcher.path().display().to_string());
        let before = policy.clone();

        assert!(withhold_unsupported_loopback(&mut policy.process_sandbox).is_none());
        assert_eq!(policy, before);
        assert!(loopback_support(&policy.process_sandbox).is_supported());
    }

    /// The spawn path applies the rule itself, so a surface that never calls
    /// the query still gets the same behavior and the same visible notice.
    #[test]
    fn the_spawn_path_drops_the_grant_and_emits_one_typed_notice() {
        super::reset_notice();
        let sink = Rc::new(CollectorSink::new());
        add_event_sink(sink.clone());
        crate::orchestration::push_execution_policy(confined_policy_granting_loopback());

        let (first, _) = crate::stdlib::sandbox::active_sandbox_policy().expect("confined");
        let (second, _) = crate::stdlib::sandbox::active_sandbox_policy().expect("confined");
        crate::orchestration::pop_execution_policy();
        reset_event_sinks();

        assert!(!first.process_sandbox.allow_tcp_loopback);
        assert!(!second.process_sandbox.allow_tcp_loopback);
        let notices: Vec<_> = sink
            .logs
            .borrow()
            .iter()
            .filter(|event| event.category == LOOPBACK_UNAVAILABLE_CATEGORY)
            .cloned()
            .collect();
        assert_eq!(
            notices.len(),
            1,
            "one notice per process, not one per spawn"
        );
        assert_eq!(notices[0].metadata["grant"], LOOPBACK_GRANT_FIELD);
        assert_eq!(notices[0].metadata["reason"], "no_launcher_configured");
    }

    #[test]
    fn the_spawn_path_keeps_the_grant_when_the_launcher_exists() {
        let sink = Rc::new(CollectorSink::new());
        add_event_sink(sink.clone());
        let launcher = launcher_file();
        let mut policy = confined_policy_granting_loopback();
        policy.process_sandbox.netns_launcher_path = Some(launcher.path().display().to_string());
        crate::orchestration::push_execution_policy(policy);

        let (active, _) = crate::stdlib::sandbox::active_sandbox_policy().expect("confined");
        crate::orchestration::pop_execution_policy();
        reset_event_sinks();

        assert!(active.process_sandbox.allow_tcp_loopback);
        assert!(sink
            .logs
            .borrow()
            .iter()
            .all(|event| event.category != LOOPBACK_UNAVAILABLE_CATEGORY));
    }
}

#[cfg(target_os = "macos")]
#[test]
fn macos_keeps_the_grant_without_a_launcher() {
    let mut policy = confined_policy_granting_loopback();
    assert!(withhold_unsupported_loopback(&mut policy.process_sandbox).is_none());
    assert!(policy.process_sandbox.allow_tcp_loopback);
}

/// A platform with no loopback mechanism must not drop the grant: its backend
/// refuses it, and dropping would turn a refusal into broader egress.
#[test]
fn an_unsupported_platform_keeps_the_grant_for_its_backend_to_refuse() {
    let unsupported = LoopbackUnavailable {
        reason: LoopbackUnavailableReason::UnsupportedPlatform,
        launcher: None,
    };
    assert!(!unsupported.is_droppable());
    let missing = LoopbackUnavailable {
        reason: LoopbackUnavailableReason::NoLauncherConfigured,
        launcher: None,
    };
    assert!(missing.is_droppable());
}
