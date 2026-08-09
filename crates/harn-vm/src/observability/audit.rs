//! `harn-obs-audit` conformance gate.
//!
//! Scans a sequence of observability events emitted by a `.harn` handler
//! (typically captured via `obs.events()` from a test harness) and
//! reports violations of the published `harn.*` schema. Run from
//! conformance tests so every primitive that lands in epic A (and every
//! cloud endpoint port in epic E) gets gated on:
//!
//! * **Vocabulary:** any attribute key under a known `harn.<ns>.*`
//!   prefix is declared in [`crate::observability::vocabulary`] —
//!   already enforced at emit time, but the audit doubles as a
//!   structural check for events that bypass the typed
//!   `harness.obs.*` surface.
//! * **Instrument tagging:** every metric event carries an
//!   `instrument` field (`counter` / `histogram` / `gauge`), so
//!   exporters can map it to the right OTel instrument without
//!   guessing.
//! * **Orphan spans:** every `span_end` event names a `trace_id` (set
//!   automatically by [`crate::stdlib::observability`]) — a missing
//!   id means the span never opened through the standard primitive.
//!
//! The audit is intentionally event-shape-based rather than AST-based:
//! tests run the handler under the `test` backend and feed the captured
//! events back through [`audit_events`]. Adding a new event class is
//! one function in this module plus the namespace edit in
//! [`crate::observability::vocabulary`].

use serde_json::Value;

use super::vocabulary;

/// One audit violation. Carries enough context for the harness to
/// surface a clickable, actionable error string in test output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuditFinding {
    pub kind: AuditFindingKind,
    /// The offending key (for attribute violations), span name, or
    /// metric name — whichever identifies the event.
    pub key: String,
    /// Which surface the offending event came from (`span`, `metric`,
    /// `log`, ...). Mirrors the `kind` field on the event.
    pub surface: String,
    /// Auxiliary context — the event's `name` or the
    /// `harn.<namespace>` that the key would be expected to live under.
    pub context: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuditFindingKind {
    /// An attribute key under a known `harn.<ns>.*` prefix that isn't
    /// declared in the published vocabulary. Catches typos in
    /// primitive emit sites that would otherwise drift into
    /// dashboards.
    UnknownVocabKey,
    /// A metric event without an `instrument` field. Metrics emitted
    /// through the typed `harness.obs.{counter,histogram,gauge}` set
    /// it automatically; raw `obs.metric(...)` calls don't, so
    /// primitives that want OTel-compatible metrics must migrate to
    /// the instrument variants.
    MetricMissingInstrument,
    /// A `span_end` event with no `trace_id`. Spans opened through
    /// [`crate::stdlib::observability::start_span_typed`] always carry
    /// one — missing means the event was hand-crafted and skipped the
    /// span helpers.
    OrphanSpan,
}

impl AuditFinding {
    /// Render the finding as a single line suitable for test output or
    /// CI logs. Always starts with `HARN-OBS-AUDIT` so log scanners can
    /// pick it out without re-parsing.
    pub fn line(&self) -> String {
        match self.kind {
            AuditFindingKind::UnknownVocabKey => format!(
                "HARN-OBS-AUDIT: {surface} `{ctx}` attribute `{key}` is not declared in the harn.* vocabulary",
                surface = self.surface,
                ctx = self.context,
                key = self.key,
            ),
            AuditFindingKind::MetricMissingInstrument => format!(
                "HARN-OBS-AUDIT: metric `{key}` lacks an `instrument` field (use harness.obs.{{counter,histogram,gauge}}; raw obs.metric() is not OTel-compatible)",
                key = self.key,
            ),
            AuditFindingKind::OrphanSpan => format!(
                "HARN-OBS-AUDIT: span `{key}` has no trace_id (span must open through harness.obs.span/start_span)",
                key = self.key,
            ),
        }
    }
}

/// Audit a sequence of obs events. The events are the payload values
/// returned by `obs.events()` / `obs.events_take()` from `.harn`.
///
/// `events` typically wraps each payload as
/// `{ backend, format, payload: {...} }` — we drill into `payload` for
/// the structural fields. Any non-object entry is skipped silently
/// (compose backends emit nested arrays).
pub fn audit_events(events: &[Value]) -> Vec<AuditFinding> {
    let mut findings = Vec::new();
    for entry in events {
        audit_one(entry, &mut findings);
    }
    findings
}

fn audit_one(entry: &Value, findings: &mut Vec<AuditFinding>) {
    let payload = entry.get("payload").unwrap_or(entry);
    let Some(map) = payload.as_object() else {
        return;
    };
    let kind = map.get("kind").and_then(Value::as_str).unwrap_or("");
    let name = map
        .get("name")
        .and_then(Value::as_str)
        .or_else(|| map.get("message").and_then(Value::as_str))
        .unwrap_or("")
        .to_string();

    if kind == "metric" && !map.contains_key("instrument") {
        findings.push(AuditFinding {
            kind: AuditFindingKind::MetricMissingInstrument,
            key: name.clone(),
            surface: "metric".to_string(),
            context: String::new(),
        });
    }

    if kind == "span_end" {
        let has_trace_id = map
            .get("trace_id")
            .and_then(Value::as_str)
            .is_some_and(|id| !id.is_empty());
        if !has_trace_id {
            findings.push(AuditFinding {
                kind: AuditFindingKind::OrphanSpan,
                key: name.clone(),
                surface: "span".to_string(),
                context: String::new(),
            });
        }
    }

    if let Some(Value::Object(fields)) = map.get("fields") {
        for key in fields.keys() {
            if vocabulary::is_violation(key) {
                findings.push(AuditFinding {
                    kind: AuditFindingKind::UnknownVocabKey,
                    key: key.clone(),
                    surface: kind.to_string(),
                    context: name.clone(),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn metric_without_instrument_is_flagged() {
        let events = vec![json!({"payload": {
            "kind": "metric",
            "name": "harn.mcp.calls",
            "value": 1,
            "fields": {},
        }})];
        let findings = audit_events(&events);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, AuditFindingKind::MetricMissingInstrument);
        assert!(findings[0].line().contains("harn.mcp.calls"));
    }

    #[test]
    fn metric_with_instrument_passes() {
        let events = vec![json!({"payload": {
            "kind": "metric",
            "name": "harn.mcp.calls",
            "value": 1,
            "instrument": "counter",
            "fields": {"harn.mcp.server": "fs"},
        }})];
        assert!(audit_events(&events).is_empty());
    }

    #[test]
    fn unknown_vocab_attribute_is_flagged() {
        let events = vec![json!({"payload": {
            "kind": "log",
            "message": "boop",
            "fields": {"harn.mcp.boops": "wat"},
        }})];
        let findings = audit_events(&events);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, AuditFindingKind::UnknownVocabKey);
        assert_eq!(findings[0].key, "harn.mcp.boops");
    }

    #[test]
    fn span_end_without_trace_id_is_flagged() {
        let events = vec![json!({"payload": {
            "kind": "span_end",
            "name": "raw_span",
            "fields": {},
        }})];
        let findings = audit_events(&events);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, AuditFindingKind::OrphanSpan);
    }

    #[test]
    fn user_attributes_outside_harn_prefix_pass() {
        let events = vec![json!({"payload": {
            "kind": "log",
            "message": "user log",
            "fields": {"user.id": 7, "custom.tag": "ok"},
        }})];
        assert!(audit_events(&events).is_empty());
    }

    #[test]
    fn compose_payload_arrays_descend_into_inner_entries() {
        // Events wrapped by the compose backend land as nested arrays
        // — the audit should still surface their inner findings rather
        // than treating the wrapper as opaque.
        let events = vec![json!({"payload": {
            "kind": "metric",
            "name": "harn.pg.queries",
            "instrument": "counter",
            "fields": {"harn.pg.bogus": "nope"},
        }})];
        let findings = audit_events(&events);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, AuditFindingKind::UnknownVocabKey);
        assert_eq!(findings[0].key, "harn.pg.bogus");
    }
}
