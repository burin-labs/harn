//! Generated projections of the Harn-owned deterministic session recap.
//!
//! The runtime DTOs and JSON schema in `harn_vm::session_recap` own meaning.
//! This module keeps every host-language projection and the non-vacuous wire
//! fixture together so adding a field cannot hide in one language emitter.

use serde_json::{json, Value};

pub(super) fn session_recap_round_trip_fixture() -> Result<Value, String> {
    let source_events = (1..=9)
        .map(|event_id| {
            json!({
                "eventId": event_id,
                "recordHash": format!("sha256:fixture-{event_id}"),
            })
        })
        .collect::<Vec<_>>();
    let wire = json!({
        "state": "available",
        "snapshot": {
            "schemaVersion": harn_vm::session_recap::SESSION_RECAP_SCHEMA_VERSION,
            "sessionId": "session-recap-fixture",
            "query": {
                "sessionId": "session-recap-fixture",
                "runId": "run-recap-fixture",
                "turnId": "turn-recap-fixture",
                "fromEventId": 1,
                "limit": 32
            },
            "cursor": {"lastEventId": 9, "nextEventId": 10},
            "coverage": {
                "scanned": 9,
                "matched": 9,
                "pending": 1,
                "unassigned": 0,
                "truncated": true
            },
            "source": {"firstEventId": 1, "lastEventId": 9, "events": source_events},
            "contentHash": "sha256:fixture-content",
            "projectionHash": "sha256:fixture-projection",
            "turns": [{
                "turnId": "turn-recap-fixture",
                "runId": "run-recap-fixture",
                "state": "complete",
                "prompts": [{"text": "Investigate the incident", "sourceEventId": 1}],
                "iterations": [{
                    "iteration": 0,
                    "state": "complete",
                    "assistantText": [{"text": "I found and fixed the cause.", "sourceEventId": 3}],
                    "tools": [{
                        "toolCallId": "tool-recap-fixture",
                        "toolName": "write_file",
                        "state": "completed",
                        "callObserved": true,
                        "resultObserved": true,
                        "input": {"path": "src/lib.rs"},
                        "output": {"changed": true},
                        "verification": {
                            "schema": "harn.agent_tool_postcondition.v1",
                            "status": "passed",
                            "verifiedPaths": ["src/lib.rs"],
                            "sourceEventId": 5
                        },
                        "sourceEventIds": [4, 5]
                    }],
                    "plans": [{
                        "documentId": "plan-recap-fixture",
                        "revisionId": "revision-recap-fixture",
                        "title": "Repair the incident",
                        "summary": "Fix and verify the owning seam",
                        "steps": [{
                            "id": "step-recap-fixture",
                            "content": "Run the regression",
                            "status": "completed"
                        }],
                        "event": {
                            "kind": "updated",
                            "eventId": "plan-event-recap-fixture",
                            "inputRevisionId": "revision-before-recap-fixture"
                        },
                        "sourceEventId": 6
                    }],
                    "progress": [{
                        "message": "Regression is green",
                        "entries": [{
                            "content": "Verify the canonical path",
                            "status": "completed",
                            "priority": "high"
                        }],
                        "replace": true,
                        "sourceEventId": 7
                    }],
                    "sourceEventIds": [2, 3, 4, 5, 6, 7, 8]
                }],
                "terminal": {
                    "state": "complete",
                    "finalStatus": "done",
                    "stopReason": "completed",
                    "kind": "natural",
                    "owner": "agent",
                    "reason": "verified",
                    "sourceEventId": 9
                },
                "sourceEventIds": [1, 2, 3, 4, 5, 6, 7, 8, 9]
            }],
            "extensions": {
                "example.harn.dev/recap": {"label": "fixture"}
            }
        }
    });
    let typed: harn_vm::session_recap::SessionRecapAvailability = serde_json::from_value(wire)
        .map_err(|error| format!("session recap fixture does not match runtime types: {error}"))?;
    serde_json::to_value(typed)
        .map_err(|error| format!("failed to encode typed session recap fixture: {error}"))
}

pub(super) fn append_typescript_session_recap_types(out: &mut String) {
    out.push_str(
        r#"
export const HARN_SESSION_RECAP_QUERY_METHOD = "harn.session_recap.query" as const
export const HARN_SESSION_RECAP_SCHEMA_VERSION = 1 as const

"#,
    );
    super::recap_records::append_enums(out, super::records::Target::Typescript);
    super::recap_records::append(out, super::records::Target::Typescript);
    out.push_str(r#"
export type HarnSessionRecapAvailability =
  | { state: "available"; snapshot: HarnSessionRecapSnapshot }
  | { state: "unavailable"; reason: HarnSessionRecapUnavailableReason }

function harnSessionRecapObject(value: unknown, label: string, keys: readonly string[]): Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new TypeError(`${label} must be an object`)
  }
  const object = value as Record<string, unknown>
  const known = new Set(keys)
  for (const key of Object.keys(object)) {
    if (!known.has(key)) throw new TypeError(`${label} contains unknown field ${key}`)
  }
  return object
}

function harnSessionRecapArray(value: unknown, label: string): unknown[] {
  if (!Array.isArray(value)) throw new TypeError(`${label} must be an array`)
  return value
}

"#,
    );
    super::recap_records::append_validators(out, super::records::Target::Typescript);
    out.push_str(r#"

/** Decode the closed recap envelope without silently dropping future fields. */
export function decodeHarnSessionRecapAvailability(value: unknown): HarnSessionRecapAvailability {
  const base = harnSessionRecapObject(value, "Harn session recap availability", ["state", "snapshot", "reason"])
  const availability = base as Record<string, unknown>
  if (availability.state === "unavailable") {
    harnSessionRecapObject(value, "Harn session recap availability", ["state", "reason"])
    return value as HarnSessionRecapAvailability
  }
  if (availability.state !== "available") {
    throw new TypeError("Harn session recap availability has an unknown state")
  }
  harnSessionRecapObject(value, "Harn session recap availability", ["state", "snapshot"])
  validateHarnSessionRecapSnapshot(availability.snapshot)
  return value as HarnSessionRecapAvailability
}
"#,
    );
}

pub(super) fn append_rust_session_recap_types(out: &mut String) {
    out.push_str(
        r#"
pub const HARN_SESSION_RECAP_QUERY_METHOD: &str = "harn.session_recap.query";
pub const HARN_SESSION_RECAP_SCHEMA_VERSION: u32 = 1;

"#,
    );
    super::recap_records::append_enums(out, super::records::Target::Rust);
    super::recap_records::append(out, super::records::Target::Rust);
    out.push_str(
        r#"
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum HarnSessionRecapAvailability {
    Available { snapshot: Box<HarnSessionRecapSnapshot> },
    Unavailable { reason: HarnSessionRecapUnavailableReason },
}
"#,
    );
}

pub(super) fn append_swift_session_recap_types(out: &mut String) {
    out.push_str(
        r#"
public enum HarnSessionRecapProtocol {
    public static let queryMethod = "harn.session_recap.query"
    public static let schemaVersion = 1
}
"#,
    );
    super::recap_records::append_enums(out, super::records::Target::Swift);
    super::recap_records::append(out, super::records::Target::Swift);
    out.push_str(
        r#"
public struct HarnSessionRecapAvailability: Codable, Sendable, Equatable {
    public var state: HarnSessionRecapAvailabilityState
    public var snapshot: HarnSessionRecapSnapshot?
    public var reason: HarnSessionRecapUnavailableReason?
}

private struct HarnSessionRecapAnyCodingKey: CodingKey {
    var stringValue: String
    var intValue: Int?
    init?(stringValue: String) { self.stringValue = stringValue; self.intValue = nil }
    init?(intValue: Int) { self.stringValue = String(intValue); self.intValue = intValue }
}

private func harnSessionRecapObject(
    _ value: HarnACPValue?,
    label: String,
    keys: Set<String>
) throws -> [String: HarnACPValue] {
    guard case .object(let object) = value else {
        throw DecodingError.dataCorrupted(
            .init(codingPath: [], debugDescription: "\(label) must be an object")
        )
    }
    if let unknown = object.keys.first(where: { !keys.contains($0) }) {
        throw DecodingError.dataCorrupted(
            .init(codingPath: [], debugDescription: "\(label) contains unknown field \(unknown)")
        )
    }
    return object
}

private func harnSessionRecapArray(_ value: HarnACPValue?, label: String) throws -> [HarnACPValue] {
    guard case .array(let array) = value else {
        throw DecodingError.dataCorrupted(
            .init(codingPath: [], debugDescription: "\(label) must be an array")
        )
    }
    return array
}

"#,
    );
    super::recap_records::append_validators(out, super::records::Target::Swift);
    out.push_str(r#"

extension HarnSessionRecapAvailability {
    private enum CodingKeys: String, CodingKey { case state, snapshot, reason }

    public init(from decoder: Decoder) throws {
        let raw = try HarnACPValue(from: decoder)
        let object = try harnSessionRecapObject(raw, label: "Harn session recap availability", keys: ["state", "snapshot", "reason"])
        let values = try decoder.container(keyedBy: CodingKeys.self)
        state = try values.decode(HarnSessionRecapAvailabilityState.self, forKey: .state)
        switch state {
        case .available:
            if Set(object.keys) != Set(["state", "snapshot"]) {
                throw DecodingError.dataCorrupted(.init(codingPath: [], debugDescription: "available Harn session recap requires only snapshot"))
            }
            try validateHarnSessionRecapSnapshot(object["snapshot"])
            snapshot = try values.decode(HarnSessionRecapSnapshot.self, forKey: .snapshot)
            reason = nil
        case .unavailable:
            if Set(object.keys) != Set(["state", "reason"]) {
                throw DecodingError.dataCorrupted(.init(codingPath: [], debugDescription: "unavailable Harn session recap requires only reason"))
            }
            snapshot = nil
            reason = try values.decode(HarnSessionRecapUnavailableReason.self, forKey: .reason)
        }
    }

    public func encode(to encoder: Encoder) throws {
        var values = encoder.container(keyedBy: CodingKeys.self)
        try values.encode(state, forKey: .state)
        switch state {
        case .available: try values.encode(snapshot, forKey: .snapshot)
        case .unavailable: try values.encode(reason, forKey: .reason)
        }
    }
}

extension HarnSessionRecapSnapshot {
    private enum CodingKeys: String, CodingKey, CaseIterable {
        case schemaVersion, sessionId, query, cursor, coverage, source
        case contentHash, projectionHash, turns, extensions
    }

    public init(from decoder: Decoder) throws {
        let raw = try decoder.container(keyedBy: HarnSessionRecapAnyCodingKey.self)
        let known = Set(CodingKeys.allCases.map(\.rawValue))
        if let unknown = raw.allKeys.first(where: { !known.contains($0.stringValue) }) {
            throw DecodingError.dataCorruptedError(
                forKey: unknown,
                in: raw,
                debugDescription: "Harn session recap snapshot contains unknown field \(unknown.stringValue)"
            )
        }
        let values = try decoder.container(keyedBy: CodingKeys.self)
        schemaVersion = try values.decode(Int.self, forKey: .schemaVersion)
        sessionId = try values.decode(String.self, forKey: .sessionId)
        query = try values.decode(HarnSessionRecapQuery.self, forKey: .query)
        cursor = try values.decode(HarnSessionRecapCursor.self, forKey: .cursor)
        coverage = try values.decode(HarnSessionRecapCoverage.self, forKey: .coverage)
        source = try values.decode(HarnSessionRecapSource.self, forKey: .source)
        contentHash = try values.decode(String.self, forKey: .contentHash)
        projectionHash = try values.decode(String.self, forKey: .projectionHash)
        turns = try values.decode([HarnSessionPromptTurnRecap].self, forKey: .turns)
        extensions = try values.decode([String: HarnACPValue].self, forKey: .extensions)
    }

    public func encode(to encoder: Encoder) throws {
        var values = encoder.container(keyedBy: CodingKeys.self)
        try values.encode(schemaVersion, forKey: .schemaVersion)
        try values.encode(sessionId, forKey: .sessionId)
        try values.encode(query, forKey: .query)
        try values.encode(cursor, forKey: .cursor)
        try values.encode(coverage, forKey: .coverage)
        try values.encode(source, forKey: .source)
        try values.encode(contentHash, forKey: .contentHash)
        try values.encode(projectionHash, forKey: .projectionHash)
        try values.encode(turns, forKey: .turns)
        try values.encode(extensions, forKey: .extensions)
    }
}
"#,
    );
}

pub(super) fn append_python_session_recap_types(out: &mut String) {
    out.push_str(
        r#"

HARN_SESSION_RECAP_QUERY_METHOD: str = "harn.session_recap.query"
HARN_SESSION_RECAP_SCHEMA_VERSION: int = 1

"#,
    );
    super::recap_records::append_enums(out, super::records::Target::Python);
    out.push_str(r#"

class _HarnStrictRecapDataclass(_HarnDataclass):
    @classmethod
    def _strict_values(cls, data: Mapping[str, Any]) -> Dict[str, Any]:
        if not isinstance(data, Mapping):
            raise TypeError(f"{cls.__name__}.from_wire expected a mapping, got {type(data).__name__}")
        expected = {item.name for item in fields(cls)}  # type: ignore[arg-type]
        unknown = set(data) - expected
        missing = expected - set(data)
        if unknown:
            raise ValueError(f"{cls.__name__} contains unknown fields: {', '.join(sorted(unknown))}")
        if missing:
            raise ValueError(f"{cls.__name__} is missing fields: {', '.join(sorted(missing))}")
        return dict(data)

    @classmethod
    def from_wire(cls: Type[_T], data: Mapping[str, Any]) -> _T:
        return cls(**cls._strict_values(data))

    def to_wire(self) -> JsonObject:
        return _harn_recap_wire(self)

def _harn_recap_wire(value: Any) -> Any:
    if is_dataclass(value):
        return _harn_recap_wire(asdict(value))
    if isinstance(value, dict):
        return {key: _harn_recap_wire(item) for key, item in value.items()}
    if isinstance(value, list):
        return [_harn_recap_wire(item) for item in value]
    if isinstance(value, Enum):
        return value.value
    return value

"#,
    );
    super::recap_records::append(out, super::records::Target::Python);
    out.push_str(r#"
@dataclass
class HarnSessionRecapAvailability(_HarnDataclass):
    state: HarnSessionRecapAvailabilityState
    snapshot: Optional[HarnSessionRecapSnapshot] = None
    reason: Optional[HarnSessionRecapUnavailableReason] = None

    @classmethod
    def from_wire(cls, data: Mapping[str, Any]) -> "HarnSessionRecapAvailability":
        if not isinstance(data, Mapping):
            raise TypeError(f"HarnSessionRecapAvailability.from_wire expected a mapping, got {type(data).__name__}")
        state = HarnSessionRecapAvailabilityState(data.get("state"))
        expected = {"state", "snapshot"} if state is HarnSessionRecapAvailabilityState.AVAILABLE else {"state", "reason"}
        unknown = set(data) - expected
        missing = expected - set(data)
        if unknown:
            raise ValueError(f"HarnSessionRecapAvailability contains unknown fields: {', '.join(sorted(unknown))}")
        if missing:
            raise ValueError(f"HarnSessionRecapAvailability is missing fields: {', '.join(sorted(missing))}")
        if state is HarnSessionRecapAvailabilityState.AVAILABLE:
            return cls(state=state, snapshot=HarnSessionRecapSnapshot.from_wire(data["snapshot"]))
        return cls(state=state, reason=HarnSessionRecapUnavailableReason(data["reason"]))

    def to_wire(self) -> JsonObject:
        if self.state is HarnSessionRecapAvailabilityState.AVAILABLE:
            if self.snapshot is None or self.reason is not None:
                raise ValueError("available Harn session recap requires only snapshot")
            return {"state": self.state.value, "snapshot": _harn_recap_wire(self.snapshot)}
        if self.reason is None or self.snapshot is not None:
            raise ValueError("unavailable Harn session recap requires only reason")
        return {"state": self.state.value, "reason": self.reason.value}
"#,
    );
}

pub(super) fn append_go_session_recap_types(out: &mut String) {
    out.push_str(
        r#"

const HarnSessionRecapQueryMethod = "harn.session_recap.query"
const HarnSessionRecapSchemaVersion uint32 = 1

"#,
    );
    super::recap_records::append_enums(out, super::records::Target::Go);
    out.push_str(
        r#"

const HarnSessionRecapVerificationPassed HarnSessionRecapVerificationStatus = "passed"

func (status *HarnSessionRecapVerificationStatus) UnmarshalJSON(data []byte) error {
	var decoded string
	if err := json.Unmarshal(data, &decoded); err != nil { return err }
	if decoded != string(HarnSessionRecapVerificationPassed) {
		return fmt.Errorf("unknown Harn session recap verification status %q", decoded)
	}
	*status = HarnSessionRecapVerificationPassed
	return nil
}

"#,
    );
    super::recap_records::append(out, super::records::Target::Go);
    out.push_str(r#"

func harnSessionRecapDecodeStrict[T any](data []byte, target *T) error {
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.DisallowUnknownFields()
	return decoder.Decode(target)
}

func (value *HarnSessionRecapQuery) UnmarshalJSON(data []byte) error { type wire HarnSessionRecapQuery; var decoded wire; if err := harnSessionRecapDecodeStrict(data, &decoded); err != nil { return err }; *value = HarnSessionRecapQuery(decoded); return nil }
func (value *HarnSessionRecapCursor) UnmarshalJSON(data []byte) error { type wire HarnSessionRecapCursor; var decoded wire; if err := harnSessionRecapDecodeStrict(data, &decoded); err != nil { return err }; *value = HarnSessionRecapCursor(decoded); return nil }
func (value *HarnSessionRecapCoverage) UnmarshalJSON(data []byte) error { type wire HarnSessionRecapCoverage; var decoded wire; if err := harnSessionRecapDecodeStrict(data, &decoded); err != nil { return err }; *value = HarnSessionRecapCoverage(decoded); return nil }
func (value *HarnSessionRecapSourceEvent) UnmarshalJSON(data []byte) error { type wire HarnSessionRecapSourceEvent; var decoded wire; if err := harnSessionRecapDecodeStrict(data, &decoded); err != nil { return err }; *value = HarnSessionRecapSourceEvent(decoded); return nil }
func (value *HarnSessionRecapSource) UnmarshalJSON(data []byte) error { type wire HarnSessionRecapSource; var decoded wire; if err := harnSessionRecapDecodeStrict(data, &decoded); err != nil { return err }; *value = HarnSessionRecapSource(decoded); return nil }
func (value *HarnSessionRecapTextFact) UnmarshalJSON(data []byte) error { type wire HarnSessionRecapTextFact; var decoded wire; if err := harnSessionRecapDecodeStrict(data, &decoded); err != nil { return err }; *value = HarnSessionRecapTextFact(decoded); return nil }
func (value *HarnSessionRecapVerificationFact) UnmarshalJSON(data []byte) error { type wire HarnSessionRecapVerificationFact; var decoded wire; if err := harnSessionRecapDecodeStrict(data, &decoded); err != nil { return err }; *value = HarnSessionRecapVerificationFact(decoded); return nil }
func (value *HarnSessionRecapToolExchange) UnmarshalJSON(data []byte) error { type wire HarnSessionRecapToolExchange; var decoded wire; if err := harnSessionRecapDecodeStrict(data, &decoded); err != nil { return err }; *value = HarnSessionRecapToolExchange(decoded); return nil }
func (value *HarnSessionRecapPlanStep) UnmarshalJSON(data []byte) error { type wire HarnSessionRecapPlanStep; var decoded wire; if err := harnSessionRecapDecodeStrict(data, &decoded); err != nil { return err }; *value = HarnSessionRecapPlanStep(decoded); return nil }
func (value *HarnSessionRecapPlanEventFact) UnmarshalJSON(data []byte) error { type wire HarnSessionRecapPlanEventFact; var decoded wire; if err := harnSessionRecapDecodeStrict(data, &decoded); err != nil { return err }; *value = HarnSessionRecapPlanEventFact(decoded); return nil }
func (value *HarnSessionRecapPlanFact) UnmarshalJSON(data []byte) error { type wire HarnSessionRecapPlanFact; var decoded wire; if err := harnSessionRecapDecodeStrict(data, &decoded); err != nil { return err }; *value = HarnSessionRecapPlanFact(decoded); return nil }
func (value *HarnSessionRecapProgressEntry) UnmarshalJSON(data []byte) error { type wire HarnSessionRecapProgressEntry; var decoded wire; if err := harnSessionRecapDecodeStrict(data, &decoded); err != nil { return err }; *value = HarnSessionRecapProgressEntry(decoded); return nil }
func (value *HarnSessionRecapProgressFact) UnmarshalJSON(data []byte) error { type wire HarnSessionRecapProgressFact; var decoded wire; if err := harnSessionRecapDecodeStrict(data, &decoded); err != nil { return err }; *value = HarnSessionRecapProgressFact(decoded); return nil }
func (value *HarnSessionRecapTerminalFact) UnmarshalJSON(data []byte) error { type wire HarnSessionRecapTerminalFact; var decoded wire; if err := harnSessionRecapDecodeStrict(data, &decoded); err != nil { return err }; *value = HarnSessionRecapTerminalFact(decoded); return nil }
func (value *HarnSessionRecapIteration) UnmarshalJSON(data []byte) error { type wire HarnSessionRecapIteration; var decoded wire; if err := harnSessionRecapDecodeStrict(data, &decoded); err != nil { return err }; *value = HarnSessionRecapIteration(decoded); return nil }
func (value *HarnSessionPromptTurnRecap) UnmarshalJSON(data []byte) error { type wire HarnSessionPromptTurnRecap; var decoded wire; if err := harnSessionRecapDecodeStrict(data, &decoded); err != nil { return err }; *value = HarnSessionPromptTurnRecap(decoded); return nil }

func (snapshot *HarnSessionRecapSnapshot) UnmarshalJSON(data []byte) error {
	type wire HarnSessionRecapSnapshot
	var decoded wire
	if err := harnSessionRecapDecodeStrict(data, &decoded); err != nil { return err }
	*snapshot = HarnSessionRecapSnapshot(decoded)
	return nil
}

type HarnSessionRecapAvailability struct { State string `json:"state"`; Snapshot *HarnSessionRecapSnapshot `json:"snapshot,omitempty"`; Reason *HarnSessionRecapUnavailableReason `json:"reason,omitempty"` }

func (value *HarnSessionRecapAvailability) UnmarshalJSON(data []byte) error {
	type wire HarnSessionRecapAvailability
	var decoded wire
	if err := harnSessionRecapDecodeStrict(data, &decoded); err != nil { return err }
	switch decoded.State {
	case "available":
		if decoded.Snapshot == nil || decoded.Reason != nil {
			return fmt.Errorf("available Harn session recap requires only snapshot")
		}
	case "unavailable":
		if decoded.Snapshot != nil || decoded.Reason == nil {
			return fmt.Errorf("unavailable Harn session recap requires only reason")
		}
	default:
		return fmt.Errorf("unknown Harn session recap availability state %q", decoded.State)
	}
	*value = HarnSessionRecapAvailability(decoded)
	return nil
}
"#,
    );
}
