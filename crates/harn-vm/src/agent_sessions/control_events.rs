//! Record an accepted control word into the session it acted on.
//!
//! This is the single write seam for
//! [`harn_session_store::SessionEventKind::Control`]. A surface that
//! accepts a stop, a steer, an interrupt or a queued note calls
//! [`record_control_event`] once, at acceptance, and every reader of
//! the session — the in-VM transcript event list, the durable event
//! stream, replay and forensics — sees the same typed row.
//!
//! Writing at acceptance rather than at delivery matters for a stop:
//! a stop unwinds the loop, so a record deferred to the next loop
//! checkpoint would never be written at all.
//!
//! The recording can fail, and its failure must stay visible. A stop
//! aimed at a session this VM thread does not own has nowhere to write,
//! and a caller that ignored that would leave "no control row in the
//! store" meaning either "no control happened" or "the control was
//! dropped". [`ControlRecordOutcome`] forces the caller to carry which
//! one it was.

use harn_session_store::ControlEvent;

use super::{append_event_to_state, SESSIONS};

/// Whether an accepted control reached the session's event stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlRecordOutcome {
    /// The typed control event was appended to the session transcript
    /// and queued for durable storage.
    Recorded,
    /// No session with this id is open on this VM thread, so the
    /// control has no event stream to land in.
    UnknownSession,
    /// The session exists but rejected the append (transcript budget).
    Rejected,
}

impl ControlRecordOutcome {
    pub fn is_recorded(self) -> bool {
        matches!(self, Self::Recorded)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Recorded => "recorded",
            Self::UnknownSession => "unknown_session",
            Self::Rejected => "rejected",
        }
    }
}

/// Append one accepted control word to `session_id`'s event stream.
///
/// Never returns an error: a surface that has already accepted a
/// control must not fail the control because the audit write missed.
/// The outcome is returned instead so the caller can publish it.
pub fn record_control_event(session_id: &str, control: &ControlEvent) -> ControlRecordOutcome {
    let event = crate::stdlib::json_to_vm_value(&control.to_payload());
    SESSIONS.with(|sessions| {
        let mut sessions = sessions.borrow_mut();
        let Some(state) = sessions.get_mut(session_id) else {
            return ControlRecordOutcome::UnknownSession;
        };
        match append_event_to_state(state, event, "record_control_event") {
            Ok(()) => ControlRecordOutcome::Recorded,
            Err(error) => {
                tracing::warn!(
                    session_id = %session_id,
                    action = control.action.as_str(),
                    %error,
                    "accepted control word could not be appended to the session event stream"
                );
                ControlRecordOutcome::Rejected
            }
        }
    })
}

/// Every typed control event currently in a session's transcript, oldest
/// first. Reads the in-memory event list, which is the same list the
/// journal projects into the durable store.
pub fn control_events(session_id: &str) -> Vec<ControlEvent> {
    SESSIONS.with(|sessions| {
        let sessions = sessions.borrow();
        let Some(state) = sessions.get(session_id) else {
            return Vec::new();
        };
        let Some(dict) = state.transcript.as_dict() else {
            return Vec::new();
        };
        let Some(crate::value::VmValue::List(events)) = dict.get("events") else {
            return Vec::new();
        };
        events
            .iter()
            .filter_map(|event| {
                ControlEvent::from_payload(&crate::llm::helpers::vm_value_to_json(event))
            })
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use harn_session_store::{ControlAction, ControlEvent, SessionEventKind};

    use super::*;

    async fn open_journaled_session(root: &std::path::Path, session_id: &str) {
        let mut options = crate::value::DictMap::new();
        crate::value::VmDictExt::put_str(&mut options, "root", root.to_string_lossy().as_ref());
        let prepared = crate::agent_session_journal::prepare(
            session_id,
            &options,
            "run-control".to_string(),
            "turn-control".to_string(),
        )
        .await
        .expect("prepare journal");
        crate::agent_sessions::open_or_create(Some(session_id.to_string()));
        crate::agent_sessions::install_journal(session_id, prepared.state)
            .expect("install journal");
    }

    async fn stored_events(
        root: &std::path::Path,
        session_id: &str,
    ) -> Vec<harn_session_store::StoredEvent> {
        let store = crate::stdlib::session_store::open_canonical_agent_session(
            &crate::stdlib::session_store::SessionStoreDir::under_root(root),
            session_id,
            None,
            harn_session_store::SessionType::User,
        )
        .await
        .expect("open canonical session");
        crate::stdlib::session_store::read_all_events(&store, session_id)
            .await
            .expect("read canonical events")
    }

    /// Falsifier. An accepted steer must reach the DURABLE event stream as
    /// a typed `control` row carrying the steer text and the caller's own
    /// mode word. Before `SessionEventKind::Control` existed there was no
    /// row to find, so this assertion is what the change has to make true.
    #[tokio::test(flavor = "current_thread")]
    async fn an_accepted_steer_is_persisted_as_a_typed_control_event() {
        crate::agent_sessions::reset_session_store();
        let root = tempfile::tempdir().expect("temp root");
        let session_id = "control-steer";
        open_journaled_session(root.path(), session_id).await;

        let outcome = record_control_event(
            session_id,
            &ControlEvent::injection(
                "session/inject",
                "ctl-steer-1",
                "accepted",
                "steer",
                "finish_step",
                "msg_inj_steer",
                "if no typed code exists, say so and stop",
            ),
        );
        assert_eq!(outcome, ControlRecordOutcome::Recorded);

        crate::agent_session_journal::flush(session_id)
            .await
            .expect("flush journal");

        let events = stored_events(root.path(), session_id).await;
        let control_rows: Vec<_> = events
            .iter()
            .filter(|event| event.kind == SessionEventKind::Control)
            .collect();
        assert_eq!(
            control_rows.len(),
            1,
            "expected exactly one typed control row; stored kinds were {:?}",
            events
                .iter()
                .map(|event| event.kind.discriminator())
                .collect::<Vec<_>>()
        );

        let typed = ControlEvent::from_stored_event(control_rows[0])
            .expect("control row must read back as a typed control event");
        assert_eq!(typed.action, ControlAction::Steer);
        assert_eq!(typed.requested_mode.as_deref(), Some("steer"));
        assert_eq!(typed.delivery_mode.as_deref(), Some("finish_step"));
        assert_eq!(typed.message_id.as_deref(), Some("msg_inj_steer"));
        assert_eq!(
            typed.text.as_deref(),
            Some("if no typed code exists, say so and stop"),
            "the steer text must survive into the store"
        );
        crate::agent_sessions::reset_session_store();
    }

    /// Falsifier. An accepted stop leaves a typed row of its own. This is
    /// the case the store had NO representation of: a cancel flipped a
    /// flag and wrote nothing.
    #[tokio::test(flavor = "current_thread")]
    async fn an_accepted_stop_is_persisted_as_a_typed_control_event() {
        crate::agent_sessions::reset_session_store();
        let root = tempfile::tempdir().expect("temp root");
        let session_id = "control-stop";
        open_journaled_session(root.path(), session_id).await;

        let outcome = record_control_event(
            session_id,
            &ControlEvent::stop("session/cancel", "ctl-stop-1", "cancelled"),
        );
        assert_eq!(outcome, ControlRecordOutcome::Recorded);
        crate::agent_session_journal::flush(session_id)
            .await
            .expect("flush journal");

        let events = stored_events(root.path(), session_id).await;
        let typed: Vec<ControlEvent> = events
            .iter()
            .filter_map(ControlEvent::from_stored_event)
            .collect();
        assert_eq!(
            typed.len(),
            1,
            "expected one typed stop row; stored rows were {:#?}",
            events
                .iter()
                .map(|event| (
                    event.kind.discriminator().to_string(),
                    event.payload.clone()
                ))
                .collect::<Vec<_>>()
        );
        assert_eq!(typed[0].action, ControlAction::Stop);
        assert_eq!(typed[0].status, "cancelled");
        assert!(
            typed[0].text.is_none(),
            "a stop carries no steer text: {typed:?}"
        );
        crate::agent_sessions::reset_session_store();
    }

    /// Negative control. A session that accepted no control word must
    /// persist ZERO control rows. Without this, a reader that classified
    /// every row as a control would pass both falsifiers above.
    #[tokio::test(flavor = "current_thread")]
    async fn a_control_free_session_persists_no_control_events() {
        crate::agent_sessions::reset_session_store();
        let root = tempfile::tempdir().expect("temp root");
        let session_id = "control-free";
        open_journaled_session(root.path(), session_id).await;

        crate::agent_sessions::inject_message(
            session_id,
            crate::stdlib::json_to_vm_value(&serde_json::json!({
                "role": "user",
                "content": "if no typed code exists, say so and stop",
            })),
        )
        .expect("inject an ordinary user message");
        crate::agent_session_journal::flush(session_id)
            .await
            .expect("flush journal");

        let events = stored_events(root.path(), session_id).await;
        // A measured zero is only meaningful if the same read returns a
        // non-zero for the rows that ARE there.
        assert!(
            !events.is_empty(),
            "the store read returned nothing at all, so the zero below would be vacuous"
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == SessionEventKind::Control)
                .count(),
            0,
            "a session with no accepted control must persist no control rows; \
             message prose that merely reads like a stop must not become one"
        );
        assert!(control_events(session_id).is_empty());
        crate::agent_sessions::reset_session_store();
    }

    /// A control aimed at a session this thread does not own has nowhere
    /// to land, and the caller is told so rather than reading the miss as
    /// a successful record.
    #[test]
    fn a_control_for_an_unknown_session_reports_the_miss() {
        crate::agent_sessions::reset_session_store();
        let outcome = record_control_event(
            "no-such-session",
            &ControlEvent::stop("session/cancel", "ctl-miss", "cancelled"),
        );
        assert_eq!(outcome, ControlRecordOutcome::UnknownSession);
        assert!(!outcome.is_recorded());
    }
}
