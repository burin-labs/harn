//! CH-04 (#1875): aggregation triggers (`batch { count, window, key, expire_action }`).
//!
//! Inngest-shape primitive — no other major durable-execution or agent
//! system has first-class fire-after-N-events. Lets a Harn trigger declare:
//!
//! ```text
//! trigger {
//!   source: "channel:pr.merged",
//!   batch: { count: 3, window: "10m", key: "repo", expire_action: "fire" },
//!   handler: ...,
//! }
//! ```
//!
//! Semantics (see issue #1875):
//! - Counter increments on each filter-passing event.
//! - Counter resets on fire or on window expire.
//! - `key`: optional JSON path into the channel payload; each distinct key
//!   value maintains its own counter + window.
//! - `expire_action`: `fire_partial` (default) flushes the partial batch
//!   when the window elapses; `discard` drops it silently.
//!
//! State lives in a per-process thread-local registry keyed by binding key
//! (id@version) and partition key. The buffer is capped at
//! [`MAX_BUFFER_EVENTS`] to keep a stuck handler from running the runtime
//! out of memory; overflow drops the oldest entries with a structured
//! warning (`triggers.aggregation.buffer_overflow`).
//!
//! Window expiration is driven by two complementary mechanisms:
//!
//! 1. **Implicit sweep**: every emit pass through
//!    [`crate::channels::dispatch_channel_emit_to_triggers`] first calls
//!    [`drain_expired`] to flush any buffers whose window has elapsed.
//! 2. **Explicit flush**: the `flush_trigger_aggregations()` builtin
//!    drains all expired buffers immediately. Tests use this together
//!    with `advance_time(ms)` to get deterministic window-expire
//!    coverage; the Rust mock clock advances `clock::now_ms` so expired
//!    buffers come through synchronously.
//!
//! Production runtimes that need a real-time fallback can also call
//! [`drain_expired`] periodically from a tokio task. v1 keeps the contract
//! synchronous-only because every dispatch path already runs through
//! `emit_channel`, so the implicit sweep covers the common case without
//! adding a background timer that would complicate replay determinism.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::Value as JsonValue;

use crate::triggers::test_util::clock;
use crate::value::{VmError, VmValue};

use super::TriggerEvent;

/// Maximum number of events buffered per (binding, partition_key). A
/// misconfigured handler that never fires would otherwise leak memory; we
/// cap the buffer and drop the *oldest* entries on overflow so the most
/// recent context survives. Overflow emits a structured warning so
/// operators can spot stuck batches.
pub const MAX_BUFFER_EVENTS: usize = 1024;

const HARN_CHN_005: &str = "HARN-CHN-005";

/// Action to take when the aggregation window elapses without reaching
/// `count`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExpireAction {
    /// Default: invoke the handler with the partial batch (length < count).
    FirePartial,
    /// Drop the buffer silently. No handler invocation, no event emitted.
    Discard,
}

impl ExpireAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FirePartial => "fire_partial",
            Self::Discard => "discard",
        }
    }
}

/// Aggregation config attached to a trigger binding. Cloned into the
/// binding at registration and read on every emit; intentionally cheap to
/// clone.
#[derive(Clone, Debug)]
pub struct TriggerAggregationConfig {
    pub count: u32,
    pub window: Duration,
    /// Dot-path into the channel payload (e.g. `"repo"`,
    /// `"pull_request.user.login"`). When `None`, all events share a
    /// single global counter for the binding.
    pub key_path: Option<String>,
    pub expire_action: ExpireAction,
}

/// A single in-memory buffer for one (binding, partition_key) pair.
#[derive(Debug)]
struct AggregationBuffer {
    events: Vec<TriggerEvent>,
    /// Wall-clock millisecond timestamp at which the window opened. Used
    /// so [`drain_expired_aggregations`] can compare against
    /// `clock::now_ms()` — honoring `mock_time(...)` /
    /// `advance_time(...)` in tests.
    window_start_ms: i64,
    window_ms: i64,
    expire_action: ExpireAction,
}

impl AggregationBuffer {
    fn new(window_ms: i64, expire_action: ExpireAction) -> Self {
        Self {
            events: Vec::new(),
            window_start_ms: clock::now_ms(),
            window_ms,
            expire_action,
        }
    }

    fn expired_at(&self, now_ms: i64) -> bool {
        now_ms.saturating_sub(self.window_start_ms) >= self.window_ms
    }
}

/// Outcome of accumulating a single event into the buffer. The caller
/// (channel fan-out) maps this to a dispatch call.
#[derive(Debug)]
pub enum AccumulateOutcome {
    /// Buffer still under threshold; nothing to dispatch yet.
    Buffered,
    /// Threshold reached; dispatch the batched events now and reset the
    /// buffer. The vector always contains exactly `count` entries — the
    /// new one plus everything previously buffered.
    Ready(Vec<TriggerEvent>),
}

/// Outcome of a window-expire sweep over a single buffer.
#[derive(Debug)]
pub struct ExpiredFlush {
    pub binding_key: String,
    pub partition_key: Option<String>,
    pub action: ExpireAction,
    pub events: Vec<TriggerEvent>,
}

#[derive(Default)]
struct AggregationRegistry {
    /// (binding_key, partition_key) → buffer. partition_key is the
    /// stringified JSON value at `key_path`, or "" when `key_path` is None.
    buffers: BTreeMap<String, BTreeMap<String, AggregationBuffer>>,
}

thread_local! {
    static REGISTRY: RefCell<AggregationRegistry> =
        RefCell::new(AggregationRegistry::default());
}

/// Reset all aggregation state. Called from the per-test reset hook so
/// buffers do not leak between tests.
pub fn clear_aggregation_state() {
    REGISTRY.with(|slot| {
        *slot.borrow_mut() = AggregationRegistry::default();
    });
}

/// Drop any buffers owned by `binding_key`. Called when a trigger binding
/// is drained or terminated so a long-lived buffer doesn't keep firing
/// after the trigger went away. Returns any events that were still
/// pending — callers can choose to flush them (currently they are
/// discarded, matching the existing trigger drain contract that
/// in-flight events stop on terminate).
pub fn drop_binding_aggregation(binding_key: &str) -> Vec<TriggerEvent> {
    REGISTRY.with(|slot| {
        let mut registry = slot.borrow_mut();
        registry
            .buffers
            .remove(binding_key)
            .into_iter()
            .flat_map(|partitions| partitions.into_values())
            .flat_map(|buffer| buffer.events.into_iter())
            .collect()
    })
}

/// Accumulate `event` into the buffer for (binding_key, partition_key).
/// Returns [`AccumulateOutcome::Ready`] when the buffer hits
/// `config.count`; the buffer is removed in that case so the next event
/// starts a fresh window.
pub fn accumulate(
    binding_key: &str,
    config: &TriggerAggregationConfig,
    partition_key: Option<&str>,
    event: TriggerEvent,
) -> AccumulateOutcome {
    let partition_key_owned = partition_key.unwrap_or("").to_string();
    let window_ms = config.window.as_millis() as i64;
    let count = config.count;
    let expire_action = config.expire_action;

    REGISTRY.with(|slot| {
        let mut registry = slot.borrow_mut();
        let partitions = registry.buffers.entry(binding_key.to_string()).or_default();
        let buffer = partitions
            .entry(partition_key_owned.clone())
            .or_insert_with(|| AggregationBuffer::new(window_ms, expire_action));

        // Enforce the per-buffer cap. We drop the *oldest* entry rather
        // than refusing the new one so the freshest event wins. Operators
        // get a warn-level structured event so a stuck batch is visible.
        if buffer.events.len() >= MAX_BUFFER_EVENTS {
            let mut overflow_meta = std::collections::BTreeMap::new();
            overflow_meta.insert("binding_key".to_string(), serde_json::json!(binding_key));
            overflow_meta.insert(
                "partition_key".to_string(),
                serde_json::json!(partition_key.unwrap_or("")),
            );
            overflow_meta.insert(
                "max_events".to_string(),
                serde_json::json!(MAX_BUFFER_EVENTS),
            );
            crate::events::log_warn_meta(
                "triggers.aggregation.buffer_overflow",
                "aggregation buffer exceeded MAX_BUFFER_EVENTS; dropping oldest entry",
                overflow_meta,
            );
            buffer.events.remove(0);
        }

        buffer.events.push(event);

        if buffer.events.len() as u32 >= count {
            let buffer = partitions
                .remove(&partition_key_owned)
                .expect("buffer just inserted");
            // Clean up the partition map when its last partition leaves
            // so the binding entry doesn't grow without bound.
            if partitions.is_empty() {
                registry.buffers.remove(binding_key);
            }
            return AccumulateOutcome::Ready(buffer.events);
        }
        AccumulateOutcome::Buffered
    })
}

/// Sweep all buffers and return entries whose window has elapsed. The
/// returned `events` vector is always non-empty for `FirePartial` (the
/// caller dispatches it as a batched event) and is also non-empty for
/// `Discard` (the caller drops it). An empty buffer never makes it into
/// the result.
///
/// Idempotent: calling repeatedly returns successive expirations only.
pub fn drain_expired_aggregations() -> Vec<ExpiredFlush> {
    let now_ms = clock::now_ms();
    REGISTRY.with(|slot| {
        let mut registry = slot.borrow_mut();
        let mut expired = Vec::new();
        let mut empty_bindings = Vec::new();
        for (binding_key, partitions) in registry.buffers.iter_mut() {
            let expired_partition_keys: Vec<String> = partitions
                .iter()
                .filter(|(_, buffer)| buffer.expired_at(now_ms) && !buffer.events.is_empty())
                .map(|(key, _)| key.clone())
                .collect();
            for partition_key in expired_partition_keys {
                let buffer = partitions
                    .remove(&partition_key)
                    .expect("partition just observed");
                let action = buffer.expire_action;
                expired.push(ExpiredFlush {
                    binding_key: binding_key.clone(),
                    partition_key: if partition_key.is_empty() {
                        None
                    } else {
                        Some(partition_key)
                    },
                    action,
                    events: buffer.events,
                });
            }
            if partitions.is_empty() {
                empty_bindings.push(binding_key.clone());
            }
        }
        for binding_key in empty_bindings {
            registry.buffers.remove(&binding_key);
        }
        expired
    })
}

/// Parse a `batch` field on a trigger config dict into a typed config.
///
/// Returns `HARN-CHN-005` on bad input: count <= 0, missing window,
/// unparseable window, unknown `expire_action`, or wrong types.
pub fn parse_aggregation_config(
    raw: &VmValue,
) -> Result<Option<TriggerAggregationConfig>, VmError> {
    let map = match raw {
        VmValue::Nil => return Ok(None),
        VmValue::Dict(map) => map,
        other => {
            return Err(VmError::Runtime(format!(
                "{HARN_CHN_005} trigger_register: `batch` must be a dict, got {}",
                other.type_name()
            )))
        }
    };

    let count = map
        .get("count")
        .ok_or_else(|| {
            VmError::Runtime(format!(
                "{HARN_CHN_005} trigger_register: batch.count is required"
            ))
        })?
        .as_int()
        .ok_or_else(|| {
            VmError::Runtime(format!(
                "{HARN_CHN_005} trigger_register: batch.count must be a positive integer"
            ))
        })?;
    if count <= 0 {
        return Err(VmError::Runtime(format!(
            "{HARN_CHN_005} trigger_register: batch.count must be greater than 0, got {count}"
        )));
    }
    let count = count as u32;

    let window_raw = match map.get("window") {
        Some(VmValue::String(text)) => text.to_string(),
        Some(other) => {
            return Err(VmError::Runtime(format!(
            "{HARN_CHN_005} trigger_register: batch.window must be a string like \"10m\", got {}",
            other.type_name()
        )))
        }
        None => {
            return Err(VmError::Runtime(format!(
                "{HARN_CHN_005} trigger_register: batch.window is required"
            )))
        }
    };
    let window = super::flow_control::parse_flow_control_duration(&window_raw).map_err(|err| {
        VmError::Runtime(format!(
            "{HARN_CHN_005} trigger_register: batch.window {err}"
        ))
    })?;

    let key_path = match map.get("key") {
        None | Some(VmValue::Nil) => None,
        Some(VmValue::String(text)) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "{HARN_CHN_005} trigger_register: batch.key must be a string JSON path, got {}",
                other.type_name()
            )))
        }
    };

    let expire_action = match map.get("expire_action") {
        None | Some(VmValue::Nil) => ExpireAction::FirePartial,
        Some(VmValue::String(text)) => match text.as_ref() {
            // Accept both names. The spec uses "fire" / "fire_partial"
            // interchangeably for the "invoke handler with N<count
            // events" case; "discard" drops the buffer.
            "fire" | "fire_partial" => ExpireAction::FirePartial,
            "discard" => ExpireAction::Discard,
            other => {
                return Err(VmError::Runtime(format!(
                    "{HARN_CHN_005} trigger_register: unknown batch.expire_action '{other}', expected fire_partial|discard"
                )))
            }
        },
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "{HARN_CHN_005} trigger_register: batch.expire_action must be a string, got {}",
                other.type_name()
            )))
        }
    };

    Ok(Some(TriggerAggregationConfig {
        count,
        window,
        key_path,
        expire_action,
    }))
}

/// Resolve the partition key for `event` against `config.key_path`.
///
/// Returns `None` when `key_path` is not set OR when the path doesn't
/// resolve in the channel payload. A missing path collapses into the
/// global ("" / `None`) bucket so a misconfigured key doesn't crash the
/// emit; this matches `SpawnToPool`'s "missing path = default" pattern.
pub fn partition_key_for_event(
    config: &TriggerAggregationConfig,
    payload: &JsonValue,
) -> Option<String> {
    let path = config.key_path.as_ref()?;
    let value = json_path_lookup(payload, path)?;
    Some(stringify_partition_key(value))
}

fn stringify_partition_key(value: &JsonValue) -> String {
    match value {
        JsonValue::String(text) => text.clone(),
        JsonValue::Null => "null".to_string(),
        JsonValue::Bool(value) => value.to_string(),
        JsonValue::Number(value) => value.to_string(),
        other => serde_json::to_string(other).unwrap_or_else(|_| "<unserializable>".to_string()),
    }
}

fn json_path_lookup<'a>(value: &'a JsonValue, path: &str) -> Option<&'a JsonValue> {
    let mut current = value;
    for segment in path.split('.') {
        if segment.is_empty() {
            return None;
        }
        current = match current {
            JsonValue::Object(map) => map.get(segment)?,
            _ => return None,
        };
    }
    Some(current)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::triggers::event::{GenericWebhookPayload, KnownProviderPayload};
    use crate::triggers::{ProviderId, ProviderPayload, SignatureStatus};
    use std::collections::BTreeMap;
    use std::time::Duration;

    fn mk_event(id: &str) -> TriggerEvent {
        TriggerEvent::new(
            ProviderId::from("channel"),
            "channel.emit",
            None,
            id.to_string(),
            None,
            BTreeMap::new(),
            ProviderPayload::Known(KnownProviderPayload::Webhook(GenericWebhookPayload {
                source: Some("aggregation-test".to_string()),
                content_type: Some("application/json".to_string()),
                raw: serde_json::json!({"id": id}),
            })),
            SignatureStatus::Unsigned,
        )
    }

    fn cfg(count: u32) -> TriggerAggregationConfig {
        TriggerAggregationConfig {
            count,
            window: Duration::from_secs(60),
            key_path: None,
            expire_action: ExpireAction::FirePartial,
        }
    }

    #[test]
    fn accumulate_fires_when_count_reached() {
        clear_aggregation_state();
        let config = cfg(3);
        for id in ["a", "b"] {
            match accumulate("t1@v1", &config, None, mk_event(id)) {
                AccumulateOutcome::Buffered => {}
                AccumulateOutcome::Ready(_) => panic!("fired too early"),
            }
        }
        let outcome = accumulate("t1@v1", &config, None, mk_event("c"));
        match outcome {
            AccumulateOutcome::Ready(events) => assert_eq!(events.len(), 3),
            AccumulateOutcome::Buffered => panic!("should have fired"),
        }
        clear_aggregation_state();
    }

    #[test]
    fn keyed_buffers_are_independent() {
        clear_aggregation_state();
        let config = cfg(2);
        let _ = accumulate("t1@v1", &config, Some("repoA"), mk_event("a1"));
        let _ = accumulate("t1@v1", &config, Some("repoB"), mk_event("b1"));
        let a2 = accumulate("t1@v1", &config, Some("repoA"), mk_event("a2"));
        let b2 = accumulate("t1@v1", &config, Some("repoB"), mk_event("b2"));
        assert!(matches!(a2, AccumulateOutcome::Ready(_)));
        assert!(matches!(b2, AccumulateOutcome::Ready(_)));
        clear_aggregation_state();
    }

    #[test]
    fn drop_binding_removes_buffers() {
        clear_aggregation_state();
        let config = cfg(5);
        let _ = accumulate("t1@v1", &config, None, mk_event("a"));
        let _ = accumulate("t1@v1", &config, None, mk_event("b"));
        let leftover = drop_binding_aggregation("t1@v1");
        assert_eq!(leftover.len(), 2);
        // Re-accumulating after drop starts fresh.
        let outcome = accumulate("t1@v1", &cfg(2), None, mk_event("c"));
        assert!(matches!(outcome, AccumulateOutcome::Buffered));
        clear_aggregation_state();
    }
}
