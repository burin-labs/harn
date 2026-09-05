//! Who the directive envelope speaks as.
//!
//! A completion-judge `continue` detail arrives as a corrective directive. It
//! is harness machinery, not the person the agent is working for, and the
//! model has to be able to tell the difference before it decides whether to
//! answer the text back to a reader. The envelope therefore declares its
//! speaker; the transport role stays `user` because that is the only
//! mid-conversation role every provider dialect accepts.

use super::reminders::{directive_envelope_message, render_pending_reminders};
use crate::llm::helpers::{
    DirectiveAuthority, ReminderPropagate, ReminderRoleHint, ReminderSource, SystemReminder,
};

fn reminder(
    role_hint: ReminderRoleHint,
    authority: DirectiveAuthority,
    body: &str,
) -> SystemReminder {
    SystemReminder {
        id: format!("reminder-{}", role_hint.as_str()),
        tags: vec!["test".to_string()],
        dedupe_key: None,
        ttl_turns: None,
        preserve_on_compact: false,
        propagate: ReminderPropagate::Session,
        role_hint,
        authority,
        source: ReminderSource::InPipeline,
        body: body.to_string(),
        fired_at_turn: 0,
        originating_agent_id: None,
    }
}

fn envelope_message(reminders: &[SystemReminder]) -> serde_json::Value {
    let rendered = render_pending_reminders(
        &crate::llm::capabilities::Capabilities::default(),
        reminders,
    );
    directive_envelope_message(&rendered).expect("a non-empty directive set renders an envelope")
}

fn envelope_text(reminders: &[SystemReminder]) -> String {
    envelope_message(reminders)["content"]
        .as_str()
        .expect("envelope content is a string")
        .to_string()
}

/// The completion judge's `continue` detail. It reaches the model as a
/// corrective directive with the default `system` role hint, and the model
/// must read it as harness machinery.
#[test]
fn a_completion_judge_directive_is_declared_harness_originated() {
    let envelope = envelope_text(&[reminder(
        ReminderRoleHint::System,
        DirectiveAuthority::Corrective,
        "The done judge says the task is not finished; keep working.",
    )]);
    assert!(
        envelope.starts_with("<context-directives speaker=\"harness\">"),
        "a corrective harness directive must open a harness-originated envelope; got:\n{envelope}"
    );
}

/// A directive a producer explicitly marks as standing in for the person keeps
/// the person's voice. Without this half, "everything is harness" would pass
/// the test above while saying nothing.
#[test]
fn a_user_block_directive_keeps_the_person_speaker() {
    let envelope = envelope_text(&[reminder(
        ReminderRoleHint::UserBlock,
        DirectiveAuthority::Contract,
        "Ship the smallest change that fixes the bug.",
    )]);
    assert!(
        envelope.starts_with("<context-directives speaker=\"person\">"),
        "a user-block directive must keep the person speaker; got:\n{envelope}"
    );
}

/// One envelope carries every pending directive, so a single harness directive
/// makes the whole envelope harness machinery.
#[test]
fn a_harness_directive_makes_a_mixed_envelope_harness_originated() {
    let envelope = envelope_text(&[
        reminder(
            ReminderRoleHint::UserBlock,
            DirectiveAuthority::Contract,
            "Ship the smallest change that fixes the bug.",
        ),
        reminder(
            ReminderRoleHint::Developer,
            DirectiveAuthority::Corrective,
            "The done judge says the task is not finished; keep working.",
        ),
    ]);
    assert!(
        envelope.starts_with("<context-directives speaker=\"harness\">"),
        "one harness directive must make the envelope harness-originated; got:\n{envelope}"
    );
}

/// The speaker is a model-visible declaration, not a wire role. Both speakers
/// still transport as `user`, because no provider dialect accepts another
/// role mid-array.
#[test]
fn both_speakers_transport_as_user() {
    for role_hint in [
        ReminderRoleHint::System,
        ReminderRoleHint::Developer,
        ReminderRoleHint::UserBlock,
        ReminderRoleHint::EphemeralCache,
    ] {
        let message = envelope_message(&[reminder(
            role_hint,
            DirectiveAuthority::Corrective,
            "keep going",
        )]);
        assert_eq!(
            message["role"],
            "user",
            "role hint {} must still transport as user",
            role_hint.as_str()
        );
    }
}
