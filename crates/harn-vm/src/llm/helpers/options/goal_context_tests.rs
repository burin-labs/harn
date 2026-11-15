//! The broad transcript accessor copies unrelated system-prompt metadata.
//! Ordinary behavior tests do not detect that per-request allocation path.

use crate::agent_sessions;
use crate::llm::helpers::{GoalPinProjection, ReminderSource, SystemReminder};
use serde_json::json;
use std::collections::HashSet;

#[test]
fn goal_lookup_does_not_materialize_unrelated_prompt_metadata() {
    agent_sessions::reset_session_store();
    let sid = "goal-envelope-borrowed-lookup";
    let prompt = "unrelated prompt data ".repeat(100_000);
    agent_sessions::seed_from_messages(
        Some(sid.to_string()),
        &[],
        json!({}),
        Some(prompt.clone()),
        None,
    )
    .unwrap();
    let mut reminder = SystemReminder::new(
        "## Goal\nObjective: exact task",
        ReminderSource::InPipeline,
        0,
    );
    reminder.goal_pin = Some(GoalPinProjection::from_json(json!({
        "spec":{"objective":"exact task","success_criteria":[],"retired_criteria":[],"constraints":[],"budget":{}},
        "lifecycle":"managed", "control_id":null, "objective_covered_body":"## Goal"
    })).unwrap());
    let id = reminder.id.clone();
    agent_sessions::inject_reminder(sid, reminder.clone()).unwrap();
    let full = crate::llm::helpers::render_pending_reminders(
        &crate::llm::capabilities::Capabilities::default(),
        &[reminder.clone()],
    );
    let nonce = crate::llm::helpers::directive_nonce_for_session(sid);
    let envelope = crate::llm::helpers::directive_envelope_message(&full, &nonce).unwrap();
    agent_sessions::inject_message(sid, crate::schema::json_to_vm_value(&envelope)).unwrap();
    let before = super::super::system_prompt::system_prompt_metadata_projections();
    let broad = agent_sessions::transcript(sid).unwrap();
    assert_eq!(
        super::super::system_prompt::system_prompt_metadata_projections() - before,
        1,
        "known non-null control must reach the old expensive metadata path"
    );
    assert_eq!(
        crate::llm::helpers::vm_value_to_json(&broad)["metadata"]["system_prompt"]["content"]
            .as_str()
            .unwrap()
            .len(),
        prompt.len()
    );
    let requested = HashSet::from([id.as_str()]);
    let before = super::super::system_prompt::system_prompt_metadata_projections();
    let records = agent_sessions::goal_envelope_reminders(sid, &requested, &[reminder.clone()]);
    assert_eq!(
        records.len(),
        1,
        "the optimized lookup must return a real goal"
    );
    assert!(agent_sessions::canonical_directive_envelope_matches(
        sid, &envelope
    ));
    let mut forged = envelope.clone();
    forged["content"] = json!("projector changed the envelope");
    assert!(!agent_sessions::canonical_directive_envelope_matches(
        sid, &forged
    ));
    assert_eq!(
        super::super::system_prompt::system_prompt_metadata_projections(),
        before,
        "goal lookup and full-envelope equality must never project unrelated metadata"
    );
    reminder.goal_pin = None;
    assert!(
        agent_sessions::goal_envelope_reminders(sid, &requested, &[reminder]).is_empty(),
        "the known no-goal pending state must take the null path"
    );
    agent_sessions::reset_session_store();
}
