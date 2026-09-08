//! What a sandbox-mechanism refusal is allowed to say, and what it must carry
//! instead.
//!
//! Harn owns the mechanism fact. The remedy sentence depends on which controls
//! an operator actually has, and an embedder that hardens its default makes the
//! fallback selector inert and may reuse a Harn ladder name for a profile of
//! its own, so any advice Harn appends about those two inverts exactly where
//! this refusal fires most.

use super::unavailable;
use super::{
    SandboxMechanism, SandboxMechanismAvailability, SandboxMechanismUnavailable, SandboxRequirement,
};
use crate::orchestration::SandboxProfile;
use crate::value::{ErrorCategory, VmValue};

/// The falsifier for the class: Harn's own text must name neither the
/// environment selector nor a profile name an embedder may have remapped.
#[test]
fn mechanism_refusal_text_names_no_embedder_owned_control() {
    let message = match unavailable(
        SandboxMechanism::LinuxLandlock,
        SandboxMechanismAvailability::AbsentOnHost,
        SandboxProfile::OsHardened,
    ) {
        Ok(_) => panic!("an OsHardened spawn must refuse a missing mechanism"),
        Err(error) => error.to_string(),
    };
    assert!(
        !message.contains("HARN_HANDLER_SANDBOX"),
        "refusal named the env selector: {message}"
    );
    assert!(
        !message.to_ascii_lowercase().contains("worktree"),
        "refusal named a profile the embedder may have remapped: {message}"
    );
    assert!(
        message.contains("Linux Landlock is not available on this host"),
        "refusal dropped the mechanism fact: {message}"
    );
}

#[test]
fn mechanism_refusal_carries_mechanism_profile_and_requirement() {
    let error = match unavailable(
        SandboxMechanism::MacosSandboxExec,
        SandboxMechanismAvailability::AbsentOnHost,
        SandboxProfile::OsHardened,
    ) {
        Ok(_) => panic!("an OsHardened spawn must refuse a missing mechanism"),
        Err(error) => error,
    };
    let refusal = error
        .sandbox_mechanism_unavailable()
        .expect("refusal must travel as a typed value, not as prose");
    assert_eq!(refusal.schema, SandboxMechanismUnavailable::SCHEMA);
    assert_eq!(refusal.mechanism, SandboxMechanism::MacosSandboxExec);
    assert_eq!(
        refusal.availability,
        SandboxMechanismAvailability::AbsentOnHost
    );
    assert_eq!(refusal.profile, SandboxProfile::OsHardened);
    assert_eq!(refusal.requirement, SandboxRequirement::Profile);
    assert_eq!(refusal.category(), ErrorCategory::ToolRejected);
    // The selector is inert under a profile that requires the mechanism, which
    // is precisely why Harn must not tell anyone to set it.
    assert!(!refusal.requirement.selector_is_honored());
}

#[test]
fn a_profile_that_honors_the_fallback_reports_the_selector_as_honored() {
    let refusal = SandboxMechanismUnavailable::new(
        SandboxMechanism::LinuxLandlock,
        SandboxMechanismAvailability::AbsentOnHost,
        SandboxProfile::Worktree,
    );
    assert_eq!(refusal.requirement, SandboxRequirement::Fallback);
    assert!(refusal.requirement.selector_is_honored());
    assert!(
        !refusal.to_string().contains("HARN_HANDLER_SANDBOX"),
        "even the honored-selector refusal states the mechanism fact alone"
    );
}

#[test]
fn caught_value_exposes_the_fields_a_consumer_would_have_parsed() {
    let error = match unavailable(
        SandboxMechanism::WindowsAppContainer,
        SandboxMechanismAvailability::EntryPointCannotAttach,
        SandboxProfile::OsHardened,
    ) {
        Ok(_) => panic!("an OsHardened spawn must refuse an unattachable mechanism"),
        Err(error) => error,
    };
    let VmValue::Dict(dict) = error.thrown_value() else {
        panic!("a caught sandbox refusal must be a dict");
    };
    let string_at = |key: &str| match dict.get(key) {
        Some(VmValue::String(value)) => value.to_string(),
        other => panic!("{key} must be a string, got {other:?}"),
    };
    assert_eq!(string_at("category"), "tool_rejected");
    assert_eq!(string_at("source"), "sandbox_mechanism");

    let Some(VmValue::Dict(cause)) = dict.get("sandbox_mechanism") else {
        panic!("a caught sandbox refusal must carry its typed cause");
    };
    let cause_string = |key: &str| match cause.get(key) {
        Some(VmValue::String(value)) => value.to_string(),
        other => panic!("{key} must be a string, got {other:?}"),
    };
    assert_eq!(cause_string("schema"), SandboxMechanismUnavailable::SCHEMA);
    assert_eq!(cause_string("mechanism"), "windows_app_container");
    assert_eq!(cause_string("availability"), "entry_point_cannot_attach");
    assert_eq!(cause_string("profile"), "os_hardened");
    assert_eq!(cause_string("requirement"), "profile");
    assert!(
        matches!(cause.get("selector_honored"), Some(VmValue::Bool(false))),
        "selector_honored must be a typed false, not prose"
    );
}

mod mechanism {
    //! The three grants that all print `Operation not permitted` must come
    //! back as three different mechanisms, and the record must say which.

    use super::super::{
        infer_process_sandbox_mechanism, ProcessSandboxMechanism, ProcessSandboxOperation,
        ProcessSandboxRefusal,
    };
    use crate::orchestration::{CapabilityPolicy, SandboxProfile};

    fn confined(workspace: &str) -> CapabilityPolicy {
        let mut policy = CapabilityPolicy {
            sandbox_profile: SandboxProfile::Worktree,
            workspace_roots: vec![workspace.to_string()],
            ..CapabilityPolicy::default()
        };
        policy.process_sandbox.allow_tcp_loopback = true;
        policy
    }

    #[test]
    fn a_unix_socket_bind_is_a_local_socket_refusal_not_egress() {
        let stderr = "org.scalasbt.ipcsocket.NativeErrorException: Error 1: Operation not permitted\n\
                      \tat org.scalasbt.ipcsocket.UnixDomainServerSocket.bind(UnixDomainServerSocket.java:88)";
        let (mechanism, grants) = infer_process_sandbox_mechanism(stderr, Some(&confined("/work")));
        assert_eq!(mechanism, ProcessSandboxMechanism::LocalSocket);
        let text = mechanism.explanation(&grants);
        assert!(
            text.contains("Unix-domain sockets are not granted"),
            "{text}"
        );
        assert!(text.contains("TCP loopback is granted"), "{text}");
    }

    #[test]
    fn a_jvm_loopback_bind_names_the_socket_boundary() {
        let stderr =
            "Exception in thread \"main\" java.net.SocketException: Operation not permitted\n\
                      \tat sun.nio.ch.Net.bind0(Native Method)";
        let (mechanism, _) = infer_process_sandbox_mechanism(stderr, None);
        assert_eq!(mechanism, ProcessSandboxMechanism::LocalSocket);
    }

    #[test]
    fn an_unresolvable_registry_host_is_egress() {
        let stderr = "sbt.librarymanagement.ResolveException: Error downloading org.scala-lang:scala-library:2.13.12\n\
                      java.net.UnknownHostException: repo1.maven.org";
        let (mechanism, _) = infer_process_sandbox_mechanism(stderr, None);
        assert_eq!(mechanism, ProcessSandboxMechanism::Egress);
    }

    #[test]
    fn a_refused_config_read_under_home_is_a_home_read() {
        let home = super::super::super::sandbox_user_home_dir().expect("home dir");
        let stderr = format!(
            "file {}/.composer/config.json is not readable.",
            home.display()
        );
        let (mechanism, _) = infer_process_sandbox_mechanism(&stderr, Some(&confined("/work")));
        assert_eq!(mechanism, ProcessSandboxMechanism::HomeRead);
    }

    #[test]
    fn a_refused_cache_write_is_a_write_even_when_it_names_home() {
        let home = super::super::super::sandbox_user_home_dir().expect("home dir");
        let stderr = format!(
            "mkdir: {}/.cache/tool: Operation not permitted",
            home.display()
        );
        let (mechanism, _) = infer_process_sandbox_mechanism(&stderr, Some(&confined("/work")));
        assert_eq!(mechanism, ProcessSandboxMechanism::Write);
    }

    #[test]
    fn a_jvm_write_to_a_home_lock_file_is_a_write_not_a_home_read() {
        let home = super::super::super::sandbox_user_home_dir().expect("home dir");
        let stderr = format!(
            "java.io.FileNotFoundException: {}/.sbt/boot/sbt.boot.lock (Operation not permitted)\n\
             \tat java.base/java.io.FileOutputStream.open0(Native Method)",
            home.display()
        );
        let (mechanism, _) = infer_process_sandbox_mechanism(&stderr, Some(&confined("/work")));
        assert_eq!(mechanism, ProcessSandboxMechanism::Write);
    }

    #[test]
    fn output_naming_no_boundary_stays_unknown() {
        let (mechanism, _) = infer_process_sandbox_mechanism("error: exit status 1", None);
        assert_eq!(mechanism, ProcessSandboxMechanism::Unknown);
    }

    #[test]
    fn the_refusal_record_and_its_projections_carry_the_mechanism() {
        let refusal = ProcessSandboxRefusal::inferred_under(
            "macos_sandbox_exec".to_string(),
            vec!["sbt".to_string(), "compile".to_string()],
            "/work".to_string(),
            "org.scalasbt.ipcsocket.NativeErrorException: Error 1: Operation not permitted",
            Some(&confined("/work")),
        );
        assert_eq!(refusal.mechanism, ProcessSandboxMechanism::LocalSocket);
        assert_eq!(refusal.operation, ProcessSandboxOperation::Unknown);
        assert!(!refusal.reason.is_empty());

        let denial = refusal.handler_denial_json();
        assert_eq!(denial["mechanism"], "local_socket");
        assert!(
            denial["reason"].as_str().unwrap().contains("Unix-domain"),
            "{denial}"
        );

        let serialized = serde_json::to_string(&refusal).unwrap();
        let round_trip: ProcessSandboxRefusal = serde_json::from_str(&serialized).unwrap();
        assert_eq!(round_trip, refusal);
    }

    #[test]
    fn a_record_written_before_the_field_existed_reads_as_unknown() {
        let legacy = serde_json::json!({
            "schema": ProcessSandboxRefusal::SCHEMA,
            "command": ["true"],
            "cwd": "/work",
            "backend": "macos_sandbox_exec",
            "operation": "unknown",
            "resource": null,
            "refused_paths": [],
            "observability": "inferred",
            "stderr_excerpt": "",
            "count": 1
        });
        let refusal: ProcessSandboxRefusal = serde_json::from_value(legacy).unwrap();
        assert_eq!(refusal.mechanism, ProcessSandboxMechanism::Unknown);
        assert!(refusal.reason.is_empty());
    }
}
