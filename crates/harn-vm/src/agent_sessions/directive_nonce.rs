use super::{
    directive_nonce, exists, record_directive_nonce, DIRECTIVE_NONCE_METADATA_KEY, SESSIONS,
};
use crate::llm::helpers::{has_directive_commit_metadata, MessageView};
use crate::value::VmValue;

/// Resolve the session's durable authority marker through its canonical writer.
/// Legacy receipts recover it from structured metadata, never message text.
pub fn directive_nonce_for_session(session_id: &str) -> String {
    static NONCES: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, String>>,
    > = std::sync::OnceLock::new();

    let recorded = directive_nonce(session_id).or_else(|| {
        SESSIONS.with(|sessions| {
            let sessions = sessions.borrow();
            let state = sessions.get(session_id)?;
            let VmValue::List(messages) = state.transcript.as_dict()?.get("messages")? else {
                return None;
            };
            messages.iter().find_map(|message| {
                has_directive_commit_metadata(message)
                    .then(|| message.str_field(DIRECTIVE_NONCE_METADATA_KEY))
                    .flatten()
                    .filter(|nonce| !nonce.is_empty())
                    .map(str::to_string)
            })
        })
    });
    if let Some(recorded) = recorded {
        let _ = record_directive_nonce(session_id, &recorded);
        return recorded;
    }
    let fresh =
        || crate::run_tape::taped_random("directive_nonce", || uuid::Uuid::now_v7().to_string());
    if exists(session_id) {
        let generated = fresh();
        return record_directive_nonce(session_id, &generated)
            .ok()
            .flatten()
            .unwrap_or(generated);
    }

    // Standalone rendering has no transcript to retain its stable marker.
    let mut nonces = NONCES
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    nonces
        .entry(session_id.to_string())
        .or_insert_with(fresh)
        .clone()
}
