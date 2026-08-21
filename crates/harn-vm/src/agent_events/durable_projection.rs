//! Deterministic projection from the full live agent-event stream to durable
//! evidence. Live consumers still receive every event; durable sinks use this
//! projector independently to avoid repeatedly writing cumulative streaming
//! tool arguments.

use std::borrow::Cow;
use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

use super::{AgentEvent, ToolCallStatus, ToolMutationStatus};

const MAX_TRACKED_STREAMS: usize = 1_024;
const MAX_TRACKED_KEY_BYTES: usize = 256 * 1_024;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct StreamKey {
    session_id: String,
    tool_call_id: String,
}

impl StreamKey {
    fn new(session_id: &str, tool_call_id: &str) -> Self {
        Self {
            session_id: session_id.to_owned(),
            tool_call_id: tool_call_id.to_owned(),
        }
    }

    fn byte_len(&self) -> usize {
        self.session_id
            .len()
            .saturating_add(self.tool_call_id.len())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SnapshotRepresentation {
    RawPartial,
    ParsedJson,
}

#[derive(Clone, Copy, Debug)]
struct StreamState {
    representation: SnapshotRepresentation,
    content_len: usize,
    content_digest: [u8; 32],
    metadata_digest: [u8; 32],
    next_checkpoint: Option<usize>,
    last_observed: u64,
}

/// Stateful, deterministic filter for durable agent-event sinks.
///
/// The live stream is intentionally not routed through this type. For a pure
/// cumulative `ToolCallUpdate(Pending)` argument stream, the first snapshot and
/// each subsequent power-of-two size crossing are retained. All other events
/// pass through. Unexpected stream behavior passes through too and re-arms the
/// geometric checkpoints from the unexpected snapshot.
/// `raw_input_partial` uses its UTF-8 bytes; `raw_input` uses compact JSON
/// serialization. Changing between those representations is unexpected and
/// therefore passes through and resets the stream.
///
/// Per-call state stores only lengths and SHA-256 digests, never argument
/// bodies. Both tracked-call count and retained identity bytes are capped;
/// least-recently observed calls leave the map first, and a stream that cannot
/// be tracked passes through fail-open.
#[derive(Default)]
pub struct DurableAgentEventProjector {
    streams: BTreeMap<StreamKey, StreamState>,
    tracked_key_bytes: usize,
    next_observation: u64,
}

impl DurableAgentEventProjector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Return whether `event` belongs in a durable sink.
    ///
    /// Callers must invoke this exactly once, in source order, before
    /// serializing or redacting an event. The decision is observer- and
    /// wall-clock-independent.
    pub fn should_persist(&mut self, event: &AgentEvent) -> bool {
        match event {
            AgentEvent::ToolCallUpdate {
                session_id,
                tool_call_id,
                tool_name,
                status,
                raw_output,
                error,
                duration_ms,
                execution_duration_ms,
                error_category,
                mutation_status,
                changed_paths,
                data,
                executor,
                parsing,
                raw_input,
                raw_input_partial,
                audit,
            } => {
                let key = StreamKey::new(session_id, tool_call_id);
                let pure_streaming_snapshot = *status == ToolCallStatus::Pending
                    && parsing.is_none()
                    && raw_output.is_none()
                    && error.is_none()
                    && duration_ms.is_none()
                    && execution_duration_ms.is_none()
                    && error_category.is_none()
                    && *mutation_status == ToolMutationStatus::Unknown
                    && changed_paths.is_none()
                    && data.is_none()
                    && executor.is_none();
                if !pure_streaming_snapshot {
                    self.remove(&key);
                    return true;
                }

                if raw_input.is_some() && raw_input_partial.is_some() {
                    self.remove(&key);
                    return true;
                }
                let snapshot = match raw_input_partial {
                    Some(partial) => Some((
                        SnapshotRepresentation::RawPartial,
                        Cow::Borrowed(partial.as_bytes()),
                    )),
                    None => match raw_input {
                        Some(parsed) => match serde_json::to_vec(parsed) {
                            Ok(serialized) => {
                                Some((SnapshotRepresentation::ParsedJson, Cow::Owned(serialized)))
                            }
                            Err(_) => {
                                self.remove(&key);
                                return true;
                            }
                        },
                        None => None,
                    },
                };
                let Some((representation, content)) = snapshot else {
                    self.remove(&key);
                    return true;
                };
                let Some(metadata_digest) = stream_metadata_digest(tool_name, audit.as_ref())
                else {
                    self.remove(&key);
                    return true;
                };
                self.observe_snapshot(key, representation, &content, metadata_digest)
            }
            AgentEvent::ToolCall {
                session_id,
                tool_call_id,
                ..
            } => {
                self.remove(&StreamKey::new(session_id, tool_call_id));
                true
            }
            AgentEvent::SessionClosed { session_id, .. } => {
                self.remove_session(session_id);
                true
            }
            _ => true,
        }
    }

    fn observe_snapshot(
        &mut self,
        key: StreamKey,
        representation: SnapshotRepresentation,
        content: &[u8],
        metadata_digest: [u8; 32],
    ) -> bool {
        let content_digest = digest(content);
        let observation = self.next_observation;
        self.next_observation = self.next_observation.saturating_add(1);
        let Some(previous) = self.streams.get_mut(&key) else {
            let state = StreamState {
                representation,
                content_len: content.len(),
                content_digest,
                metadata_digest,
                next_checkpoint: next_checkpoint_after(content.len()),
                last_observed: observation,
            };
            self.admit(key, state);
            return true;
        };
        previous.last_observed = observation;

        let metadata_changed = previous.metadata_digest != metadata_digest;
        let representation_changed = previous.representation != representation;
        let shrank = content.len() < previous.content_len;
        let same_length_mutation =
            content.len() == previous.content_len && content_digest != previous.content_digest;
        let non_prefix_growth = content.len() > previous.content_len
            && digest(&content[..previous.content_len]) != previous.content_digest;
        if metadata_changed
            || representation_changed
            || shrank
            || same_length_mutation
            || non_prefix_growth
        {
            previous.representation = representation;
            previous.content_len = content.len();
            previous.content_digest = content_digest;
            previous.metadata_digest = metadata_digest;
            previous.next_checkpoint = next_checkpoint_after(content.len());
            return true;
        }

        if content.len() == previous.content_len {
            return false;
        }

        previous.content_len = content.len();
        previous.content_digest = content_digest;
        let crossed_checkpoint = previous
            .next_checkpoint
            .is_none_or(|checkpoint| content.len() >= checkpoint);
        if crossed_checkpoint {
            previous.next_checkpoint = next_checkpoint_after(content.len());
        }
        crossed_checkpoint
    }

    fn admit(&mut self, key: StreamKey, state: StreamState) {
        let key_bytes = key.byte_len();
        if key_bytes > MAX_TRACKED_KEY_BYTES {
            return;
        }
        while self.streams.len() >= MAX_TRACKED_STREAMS
            || self.tracked_key_bytes.saturating_add(key_bytes) > MAX_TRACKED_KEY_BYTES
        {
            let Some(oldest) = self
                .streams
                .iter()
                .min_by_key(|(key, state)| (state.last_observed, *key))
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            self.remove(&oldest);
        }
        self.tracked_key_bytes = self.tracked_key_bytes.saturating_add(key_bytes);
        self.streams.insert(key, state);
    }

    fn remove(&mut self, key: &StreamKey) {
        if self.streams.remove(key).is_some() {
            self.tracked_key_bytes = self.tracked_key_bytes.saturating_sub(key.byte_len());
        }
    }

    fn remove_session(&mut self, session_id: &str) {
        let removed = self
            .streams
            .keys()
            .filter(|key| key.session_id == session_id)
            .cloned()
            .collect::<Vec<_>>();
        for key in removed {
            self.remove(&key);
        }
    }

    #[cfg(test)]
    pub(crate) fn tracked_stream_count(&self) -> usize {
        self.streams.len()
    }

    #[cfg(test)]
    pub(crate) fn max_tracked_streams(&self) -> usize {
        MAX_TRACKED_STREAMS
    }
}

fn stream_metadata_digest(
    tool_name: &str,
    audit: Option<&crate::orchestration::MutationSessionRecord>,
) -> Option<[u8; 32]> {
    let mut hasher = Sha256::new();
    hasher.update(tool_name.len().to_le_bytes());
    hasher.update(tool_name.as_bytes());
    match audit {
        Some(audit) => {
            hasher.update([1]);
            hasher.update(serde_json::to_vec(audit).ok()?);
        }
        None => hasher.update([0]),
    }
    Some(hasher.finalize().into())
}

fn digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn next_checkpoint_after(size: usize) -> Option<usize> {
    if size == 0 {
        return Some(1);
    }
    if size.is_power_of_two() {
        size.checked_mul(2)
    } else {
        size.checked_next_power_of_two()
    }
}
