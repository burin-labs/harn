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
