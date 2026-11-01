#[test]
fn guardian_preserves_payload_loader_controls_without_exposing_them_to_setup() {
    let inherited = || vec![(OsString::from("LD_BIND_NOW"), OsString::from("1"))];
    let mut command = Command::new("/bin/true");
    let expected = (b"LD_BIND_NOW".to_vec(), Some(b"1".to_vec()));
    assert_eq!(
        payload_environment(&command, false, inherited()),
        vec![expected]
    );
    assert!(payload_environment(&command, true, inherited()).is_empty());
    command.env_remove("LD_BIND_NOW");
    let removed = payload_environment(&command, false, inherited());
    assert_eq!(removed.last(), Some(&(b"LD_BIND_NOW".to_vec(), None)));
    let mut guardian = Command::new("/bin/true");
    strip_sensitive_parent_env(&mut guardian, inherited());
    assert_eq!(
        guardian.get_envs().collect::<Vec<_>>(),
        vec![(std::ffi::OsStr::new("LD_BIND_NOW"), None)]
    );
}

use std::collections::BTreeMap;

use super::*;
use crate::process::{EnvMode, OutputCapture, OwnerDeathPolicy};

#[cfg(target_os = "linux")]
#[test]
fn guardian_request_refuses_overlapping_descriptor_custody_without_closing_the_descriptor() {
    use std::os::fd::AsRawFd;

    let retained = tempfile::tempfile().unwrap();
    let descriptor = retained.as_raw_fd();
    let mut request = GuardianCommandRequest::from_command(
        &Command::new("/usr/bin/true"),
        true,
        "duplicate-descriptor-control".to_string(),
        Some(GuardianConfinement::Bubblewrap {
            descriptors: vec![descriptor],
        }),
        false,
    );
    request.pinned_verifier_descriptors = vec![descriptor];
    let request: GuardianCommandRequest =
        serde_json::from_slice(&serde_json::to_vec(&request).unwrap()).unwrap();
    // SAFETY: repeated numbers are rejected before adoption. No accepted
    // descriptor is supplied, and the retained file keeps its custody.
    let Err(error) = (unsafe { PreparedCommand::from_inherited_request(request) }) else {
        panic!("a cross-set duplicate must never construct a payload command");
    };
    assert!(error
        .to_string()
        .contains("repeated confinement descriptor"));
    retained
        .metadata()
        .expect("refused wire input must not close an existing descriptor");
}

#[cfg(target_os = "linux")]
#[test]
fn guardian_request_refuses_verifier_aliases_of_the_reserved_ruleset_role() {
    for confinement in [
        GuardianConfinement::BeforeExec {
            seccomp: Vec::new(),
            ruleset: true,
        },
        GuardianConfinement::AfterNamespace { ruleset: true },
    ] {
        let mut request = GuardianCommandRequest::from_command(
            &Command::new("/usr/bin/true"),
            true,
            "ruleset-alias-control".to_string(),
            Some(confinement),
            false,
        );
        request.pinned_verifier_descriptors = vec![RULESET_FD];
        let request: GuardianCommandRequest =
            serde_json::from_slice(&serde_json::to_vec(&request).unwrap()).unwrap();
        // SAFETY: duplicate roles are rejected before inspecting or adopting
        // the descriptor; this test supplies no accepted transfer.
        let error = unsafe { PreparedCommand::from_inherited_request(request) }
            .err()
            .expect("the ruleset role cannot also be verifier material");
        assert!(error
            .to_string()
            .contains("repeated confinement descriptor"));
    }
}

#[cfg(target_os = "linux")]
#[test]
fn guardian_request_refuses_invalid_inherited_descriptors() {
    for descriptor in [-1, 0, 1, 2, i32::MAX] {
        for verifier in [false, true] {
            let mut request = GuardianCommandRequest::from_command(
                &Command::new("/usr/bin/true"),
                true,
                "invalid-descriptor-control".to_string(),
                None,
                false,
            );
            if verifier {
                request.pinned_verifier_descriptors = vec![descriptor];
            } else {
                request.confinement = Some(GuardianConfinement::Bubblewrap {
                    descriptors: vec![descriptor],
                });
            }
            let request: GuardianCommandRequest =
                serde_json::from_slice(&serde_json::to_vec(&request).unwrap()).unwrap();
            // SAFETY: I/O slots and negative/closed numbers are refused
            // before any ownership is taken; no accepted fd is supplied.
            assert!(unsafe { PreparedCommand::from_inherited_request(request) }.is_err());
        }
    }
}

#[test]
fn guardian_request_keeps_explicit_credentials_out_of_argv_and_env() {
    // What this asserts is that an explicit credential travels over the
    // private pipe rather than through argv or the environment. It
    // inherits otherwise, and since harn#8477 an inheriting spawn states
    // that rather than getting it from the absence of a policy.
    let _environment = crate::process::test_support::declare_inherited();
    let canary = "guardian-request-pipe-canary";
    let spec = SpawnSpec {
        builtin: "guardian_request_test",
        program: "/usr/bin/env".to_string(),
        args: Vec::new(),
        cwd: None,
        env: BTreeMap::from([("EXAMPLE_API_KEY".to_string(), canary.to_string())]),
        env_remove: Vec::new(),
        env_mode: EnvMode::Patch,
        use_stdin: false,
        configure_process_group: true,
        owner_death: OwnerDeathPolicy::KillContainment,
        output_capture: OutputCapture::Pipe,
    };

    let cleanup_token = harn_vm::op_interrupt::new_process_cleanup_token();
    let PreparedGuardian {
        command: guardian,
        request,
        ..
    } = prepare_guardian(&spec, cleanup_token.clone()).expect("prepare guardian");
    harn_vm::op_interrupt::remove_process_owner_group_journal(&cleanup_token);
    let decoded: GuardianCommandRequest =
        serde_json::from_slice(&request).expect("decode private guardian request");
    assert!(
        decoded.env.iter().any(|(key, value)| {
            key.as_slice() == b"EXAMPLE_API_KEY" && value.as_deref() == Some(canary.as_bytes())
        }),
        "the private request must still carry the explicit child credential"
    );
    assert!(
        guardian
            .get_args()
            .all(|arg| !arg.to_string_lossy().contains(canary)),
        "the guardian argv must not carry child credentials"
    );
    assert!(
        guardian.get_envs().all(|(key, value)| {
            !key.to_string_lossy().contains(canary)
                && !value.is_some_and(|value| value.to_string_lossy().contains(canary))
        }),
        "the guardian environment must not carry child credentials"
    );
}

#[test]
fn guardian_scrubs_sensitive_values_inherited_from_its_parent() {
    let mut guardian = Command::new("/usr/bin/true");
    strip_sensitive_parent_env(
        &mut guardian,
        [
            (
                OsString::from("EXAMPLE_API_KEY"),
                OsString::from("secret-canary"),
            ),
            (OsString::from("PATH"), OsString::from("/usr/bin")),
            (OsString::from("LD_BIND_NOW"), OsString::from("1")),
        ],
    );

    let env = guardian.get_envs().collect::<Vec<_>>();
    assert!(env.iter().any(|(key, value)| *key == OsStr::new("LD_BIND_NOW") && value.is_none()),
        "trusted guardian setup removes parent loader controls while the payload environment remains in its pipe request");
    assert!(
        env.iter()
            .any(|(key, value)| { *key == OsStr::new("EXAMPLE_API_KEY") && value.is_none() }),
        "the guardian must remove inherited credentials before re-exec"
    );
    assert!(
        env.iter().all(|(key, _)| *key != OsStr::new("PATH")),
        "the guardian must preserve ordinary inherited environment"
    );
}
