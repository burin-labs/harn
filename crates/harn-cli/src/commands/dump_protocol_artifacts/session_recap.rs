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

export type HarnSessionRecapCompletionState = "open" | "complete" | "incomplete" | "unassigned"
export type HarnSessionRecapToolState = "open" | "completed" | "failed" | "incomplete"
export type HarnSessionRecapPlanStepStatus = "pending" | "in_progress" | "completed" | "blocked" | "cancelled"
export type HarnSessionRecapPlanEventKind = "created" | "updated"
export type HarnSessionRecapProgressStatus = "pending" | "in_progress" | "completed"
export type HarnSessionRecapProgressPriority = "high" | "medium" | "low"
export type HarnSessionRecapUnavailableReason = "journal_unavailable" | "session_missing" | "projection_failed" | "admission_terminal"

export interface HarnSessionRecapQuery {
  sessionId: string
  runId: string | null
  turnId: string | null
  fromEventId: number | null
  limit: number | null
}
export interface HarnSessionRecapCursor { lastEventId: number | null; nextEventId: number | null }
export interface HarnSessionRecapCoverage { scanned: number; matched: number; pending: number; unassigned: number; truncated: boolean }
export interface HarnSessionRecapSourceEvent { eventId: number; recordHash: string }
export interface HarnSessionRecapSource { firstEventId: number | null; lastEventId: number | null; events: HarnSessionRecapSourceEvent[] }
export interface HarnSessionRecapTextFact { text: string; sourceEventId: number }
export interface HarnSessionRecapVerificationFact { schema: string; status: string; verifiedPaths: string[]; sourceEventId: number }
export interface HarnSessionRecapToolExchange {
  toolCallId: string
  toolName: string | null
  state: HarnSessionRecapToolState
  callObserved: boolean
  resultObserved: boolean
  input: ACPValue | null
  output: ACPValue | null
  verification: HarnSessionRecapVerificationFact | null
  sourceEventIds: number[]
}
export interface HarnSessionRecapPlanStep { id: string; content: string; status: HarnSessionRecapPlanStepStatus }
export interface HarnSessionRecapPlanEventFact { kind: HarnSessionRecapPlanEventKind; eventId: string; inputRevisionId: string | null }
export interface HarnSessionRecapPlanFact {
  documentId: string; revisionId: string; title: string; summary: string
  steps: HarnSessionRecapPlanStep[]; event: HarnSessionRecapPlanEventFact | null; sourceEventId: number
}
export interface HarnSessionRecapProgressEntry { content: string; status: HarnSessionRecapProgressStatus; priority: HarnSessionRecapProgressPriority | null }
export interface HarnSessionRecapProgressFact { message: string | null; entries: HarnSessionRecapProgressEntry[]; replace: boolean; sourceEventId: number }
export interface HarnSessionRecapTerminalFact {
  state: HarnSessionRecapCompletionState; finalStatus: string | null; stopReason: string | null
  kind: string | null; owner: string | null; reason: string | null; sourceEventId: number
}
export interface HarnSessionRecapIteration {
  iteration: number | null; state: HarnSessionRecapCompletionState
  assistantText: HarnSessionRecapTextFact[]; tools: HarnSessionRecapToolExchange[]
  plans: HarnSessionRecapPlanFact[]; progress: HarnSessionRecapProgressFact[]; sourceEventIds: number[]
}
export interface HarnSessionPromptTurnRecap {
  turnId: string; runId: string; state: HarnSessionRecapCompletionState
  prompts: HarnSessionRecapTextFact[]; iterations: HarnSessionRecapIteration[]
  terminal: HarnSessionRecapTerminalFact | null; sourceEventIds: number[]
}
export interface HarnSessionRecapSnapshot {
  schemaVersion: number; sessionId: string; query: HarnSessionRecapQuery
  cursor: HarnSessionRecapCursor; coverage: HarnSessionRecapCoverage; source: HarnSessionRecapSource
  contentHash: string; projectionHash: string; turns: HarnSessionPromptTurnRecap[]
  extensions: Record<string, ACPValue>
}
export type HarnSessionRecapAvailability =
  | { state: "available"; snapshot: HarnSessionRecapSnapshot }
  | { state: "unavailable"; reason: HarnSessionRecapUnavailableReason }
"#,
    );
}

pub(super) fn append_rust_session_recap_types(out: &mut String) {
    out.push_str(
        r#"
pub const HARN_SESSION_RECAP_QUERY_METHOD: &str = "harn.session_recap.query";
pub const HARN_SESSION_RECAP_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnSessionRecapCompletionState { Open, Complete, Incomplete, Unassigned }
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnSessionRecapToolState { Open, Completed, Failed, Incomplete }
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnSessionRecapPlanStepStatus { Pending, InProgress, Completed, Blocked, Cancelled }
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnSessionRecapPlanEventKind { Created, Updated }
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnSessionRecapProgressStatus { Pending, InProgress, Completed }
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnSessionRecapProgressPriority { High, Medium, Low }
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnSessionRecapUnavailableReason { JournalUnavailable, SessionMissing, ProjectionFailed, AdmissionTerminal }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HarnSessionRecapQuery { pub session_id: String, pub run_id: Option<String>, pub turn_id: Option<String>, pub from_event_id: Option<u64>, pub limit: Option<usize> }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HarnSessionRecapCursor { pub last_event_id: Option<u64>, pub next_event_id: Option<u64> }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HarnSessionRecapCoverage { pub scanned: usize, pub matched: usize, pub pending: usize, pub unassigned: usize, pub truncated: bool }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HarnSessionRecapSourceEvent { pub event_id: u64, pub record_hash: String }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HarnSessionRecapSource { pub first_event_id: Option<u64>, pub last_event_id: Option<u64>, pub events: Vec<HarnSessionRecapSourceEvent> }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HarnSessionRecapTextFact { pub text: String, pub source_event_id: u64 }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HarnSessionRecapVerificationFact { pub schema: String, pub status: String, pub verified_paths: Vec<String>, pub source_event_id: u64 }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HarnSessionRecapToolExchange {
    pub tool_call_id: String, pub tool_name: Option<String>, pub state: HarnSessionRecapToolState,
    pub call_observed: bool, pub result_observed: bool, pub input: Option<Value>, pub output: Option<Value>,
    pub verification: Option<HarnSessionRecapVerificationFact>, pub source_event_ids: Vec<u64>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HarnSessionRecapPlanStep { pub id: String, pub content: String, pub status: HarnSessionRecapPlanStepStatus }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HarnSessionRecapPlanEventFact { pub kind: HarnSessionRecapPlanEventKind, pub event_id: String, pub input_revision_id: Option<String> }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HarnSessionRecapPlanFact {
    pub document_id: String, pub revision_id: String, pub title: String, pub summary: String,
    pub steps: Vec<HarnSessionRecapPlanStep>, pub event: Option<HarnSessionRecapPlanEventFact>, pub source_event_id: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HarnSessionRecapProgressEntry { pub content: String, pub status: HarnSessionRecapProgressStatus, pub priority: Option<HarnSessionRecapProgressPriority> }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HarnSessionRecapProgressFact { pub message: Option<String>, pub entries: Vec<HarnSessionRecapProgressEntry>, pub replace: bool, pub source_event_id: u64 }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HarnSessionRecapTerminalFact {
    pub state: HarnSessionRecapCompletionState, pub final_status: Option<String>, pub stop_reason: Option<String>,
    pub kind: Option<String>, pub owner: Option<String>, pub reason: Option<String>, pub source_event_id: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HarnSessionRecapIteration {
    pub iteration: Option<i64>, pub state: HarnSessionRecapCompletionState,
    pub assistant_text: Vec<HarnSessionRecapTextFact>, pub tools: Vec<HarnSessionRecapToolExchange>,
    pub plans: Vec<HarnSessionRecapPlanFact>, pub progress: Vec<HarnSessionRecapProgressFact>, pub source_event_ids: Vec<u64>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HarnSessionPromptTurnRecap {
    pub turn_id: String, pub run_id: String, pub state: HarnSessionRecapCompletionState,
    pub prompts: Vec<HarnSessionRecapTextFact>, pub iterations: Vec<HarnSessionRecapIteration>,
    pub terminal: Option<HarnSessionRecapTerminalFact>, pub source_event_ids: Vec<u64>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HarnSessionRecapSnapshot {
    pub schema_version: u32, pub session_id: String, pub query: HarnSessionRecapQuery,
    pub cursor: HarnSessionRecapCursor, pub coverage: HarnSessionRecapCoverage, pub source: HarnSessionRecapSource,
    pub content_hash: String, pub projection_hash: String, pub turns: Vec<HarnSessionPromptTurnRecap>,
    #[serde(default)] pub extensions: std::collections::BTreeMap<String, Value>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
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
public enum HarnSessionRecapCompletionState: String, Codable, Sendable, Equatable { case open, complete, incomplete, unassigned }
public enum HarnSessionRecapToolState: String, Codable, Sendable, Equatable { case open, completed, failed, incomplete }
public enum HarnSessionRecapPlanStepStatus: String, Codable, Sendable, Equatable { case pending, inProgress = "in_progress", completed, blocked, cancelled }
public enum HarnSessionRecapPlanEventKind: String, Codable, Sendable, Equatable { case created, updated }
public enum HarnSessionRecapProgressStatus: String, Codable, Sendable, Equatable { case pending, inProgress = "in_progress", completed }
public enum HarnSessionRecapProgressPriority: String, Codable, Sendable, Equatable { case high, medium, low }
public enum HarnSessionRecapUnavailableReason: String, Codable, Sendable, Equatable { case journalUnavailable = "journal_unavailable", sessionMissing = "session_missing", projectionFailed = "projection_failed", admissionTerminal = "admission_terminal" }
public enum HarnSessionRecapAvailabilityState: String, Codable, Sendable, Equatable { case available, unavailable }

public struct HarnSessionRecapQuery: Codable, Sendable, Equatable { public var sessionId: String; public var runId: String?; public var turnId: String?; public var fromEventId: Int?; public var limit: Int? }
public struct HarnSessionRecapCursor: Codable, Sendable, Equatable { public var lastEventId: Int?; public var nextEventId: Int? }
public struct HarnSessionRecapCoverage: Codable, Sendable, Equatable { public var scanned: Int; public var matched: Int; public var pending: Int; public var unassigned: Int; public var truncated: Bool }
public struct HarnSessionRecapSourceEvent: Codable, Sendable, Equatable { public var eventId: Int; public var recordHash: String }
public struct HarnSessionRecapSource: Codable, Sendable, Equatable { public var firstEventId: Int?; public var lastEventId: Int?; public var events: [HarnSessionRecapSourceEvent] }
public struct HarnSessionRecapTextFact: Codable, Sendable, Equatable { public var text: String; public var sourceEventId: Int }
public struct HarnSessionRecapVerificationFact: Codable, Sendable, Equatable { public var schema: String; public var status: String; public var verifiedPaths: [String]; public var sourceEventId: Int }
public struct HarnSessionRecapToolExchange: Codable, Sendable, Equatable {
    public var toolCallId: String; public var toolName: String?; public var state: HarnSessionRecapToolState
    public var callObserved: Bool; public var resultObserved: Bool; public var input: HarnACPValue?; public var output: HarnACPValue?
    public var verification: HarnSessionRecapVerificationFact?; public var sourceEventIds: [Int]
}
public struct HarnSessionRecapPlanStep: Codable, Sendable, Equatable { public var id: String; public var content: String; public var status: HarnSessionRecapPlanStepStatus }
public struct HarnSessionRecapPlanEventFact: Codable, Sendable, Equatable { public var kind: HarnSessionRecapPlanEventKind; public var eventId: String; public var inputRevisionId: String? }
public struct HarnSessionRecapPlanFact: Codable, Sendable, Equatable {
    public var documentId: String; public var revisionId: String; public var title: String; public var summary: String
    public var steps: [HarnSessionRecapPlanStep]; public var event: HarnSessionRecapPlanEventFact?; public var sourceEventId: Int
}
public struct HarnSessionRecapProgressEntry: Codable, Sendable, Equatable { public var content: String; public var status: HarnSessionRecapProgressStatus; public var priority: HarnSessionRecapProgressPriority? }
public struct HarnSessionRecapProgressFact: Codable, Sendable, Equatable { public var message: String?; public var entries: [HarnSessionRecapProgressEntry]; public var replace: Bool; public var sourceEventId: Int }
public struct HarnSessionRecapTerminalFact: Codable, Sendable, Equatable {
    public var state: HarnSessionRecapCompletionState; public var finalStatus: String?; public var stopReason: String?
    public var kind: String?; public var owner: String?; public var reason: String?; public var sourceEventId: Int
}
public struct HarnSessionRecapIteration: Codable, Sendable, Equatable {
    public var iteration: Int?; public var state: HarnSessionRecapCompletionState
    public var assistantText: [HarnSessionRecapTextFact]; public var tools: [HarnSessionRecapToolExchange]
    public var plans: [HarnSessionRecapPlanFact]; public var progress: [HarnSessionRecapProgressFact]; public var sourceEventIds: [Int]
}
public struct HarnSessionPromptTurnRecap: Codable, Sendable, Equatable {
    public var turnId: String; public var runId: String; public var state: HarnSessionRecapCompletionState
    public var prompts: [HarnSessionRecapTextFact]; public var iterations: [HarnSessionRecapIteration]
    public var terminal: HarnSessionRecapTerminalFact?; public var sourceEventIds: [Int]
}
public struct HarnSessionRecapSnapshot: Codable, Sendable, Equatable {
    public var schemaVersion: Int; public var sessionId: String; public var query: HarnSessionRecapQuery
    public var cursor: HarnSessionRecapCursor; public var coverage: HarnSessionRecapCoverage; public var source: HarnSessionRecapSource
    public var contentHash: String; public var projectionHash: String; public var turns: [HarnSessionPromptTurnRecap]
    public var extensions: [String: HarnACPValue]
}
public struct HarnSessionRecapAvailability: Codable, Sendable, Equatable {
    public var state: HarnSessionRecapAvailabilityState
    public var snapshot: HarnSessionRecapSnapshot?
    public var reason: HarnSessionRecapUnavailableReason?
}
"#,
    );
}

pub(super) fn append_python_session_recap_types(out: &mut String) {
    out.push_str(
        r#"

HARN_SESSION_RECAP_QUERY_METHOD: str = "harn.session_recap.query"
HARN_SESSION_RECAP_SCHEMA_VERSION: int = 1

class HarnSessionRecapCompletionState(str, Enum):
    OPEN = "open"
    COMPLETE = "complete"
    INCOMPLETE = "incomplete"
    UNASSIGNED = "unassigned"
class HarnSessionRecapToolState(str, Enum):
    OPEN = "open"
    COMPLETED = "completed"
    FAILED = "failed"
    INCOMPLETE = "incomplete"
class HarnSessionRecapPlanStepStatus(str, Enum):
    PENDING = "pending"
    IN_PROGRESS = "in_progress"
    COMPLETED = "completed"
    BLOCKED = "blocked"
    CANCELLED = "cancelled"
class HarnSessionRecapPlanEventKind(str, Enum):
    CREATED = "created"
    UPDATED = "updated"
class HarnSessionRecapProgressStatus(str, Enum):
    PENDING = "pending"
    IN_PROGRESS = "in_progress"
    COMPLETED = "completed"
class HarnSessionRecapProgressPriority(str, Enum):
    HIGH = "high"
    MEDIUM = "medium"
    LOW = "low"
class HarnSessionRecapUnavailableReason(str, Enum):
    JOURNAL_UNAVAILABLE = "journal_unavailable"
    SESSION_MISSING = "session_missing"
    PROJECTION_FAILED = "projection_failed"
    ADMISSION_TERMINAL = "admission_terminal"

@dataclass
class HarnSessionRecapQuery(_HarnDataclass):
    sessionId: str; runId: Optional[str]; turnId: Optional[str]; fromEventId: Optional[int]; limit: Optional[int]
@dataclass
class HarnSessionRecapCursor(_HarnDataclass):
    lastEventId: Optional[int]; nextEventId: Optional[int]
@dataclass
class HarnSessionRecapCoverage(_HarnDataclass):
    scanned: int; matched: int; pending: int; unassigned: int; truncated: bool
@dataclass
class HarnSessionRecapSourceEvent(_HarnDataclass):
    eventId: int; recordHash: str
@dataclass
class HarnSessionRecapSource(_HarnDataclass):
    firstEventId: Optional[int]; lastEventId: Optional[int]; events: List[HarnSessionRecapSourceEvent]
@dataclass
class HarnSessionRecapTextFact(_HarnDataclass):
    text: str; sourceEventId: int
@dataclass
class HarnSessionRecapVerificationFact(_HarnDataclass):
    schema: str; status: str; verifiedPaths: List[str]; sourceEventId: int
@dataclass
class HarnSessionRecapToolExchange(_HarnDataclass):
    toolCallId: str; toolName: Optional[str]; state: HarnSessionRecapToolState; callObserved: bool
    resultObserved: bool; input: Optional[JsonValue]; output: Optional[JsonValue]
    verification: Optional[HarnSessionRecapVerificationFact]; sourceEventIds: List[int]
@dataclass
class HarnSessionRecapPlanStep(_HarnDataclass):
    id: str; content: str; status: HarnSessionRecapPlanStepStatus
@dataclass
class HarnSessionRecapPlanEventFact(_HarnDataclass):
    kind: HarnSessionRecapPlanEventKind; eventId: str; inputRevisionId: Optional[str]
@dataclass
class HarnSessionRecapPlanFact(_HarnDataclass):
    documentId: str; revisionId: str; title: str; summary: str; steps: List[HarnSessionRecapPlanStep]
    event: Optional[HarnSessionRecapPlanEventFact]; sourceEventId: int
@dataclass
class HarnSessionRecapProgressEntry(_HarnDataclass):
    content: str; status: HarnSessionRecapProgressStatus; priority: Optional[HarnSessionRecapProgressPriority]
@dataclass
class HarnSessionRecapProgressFact(_HarnDataclass):
    message: Optional[str]; entries: List[HarnSessionRecapProgressEntry]; replace: bool; sourceEventId: int
@dataclass
class HarnSessionRecapTerminalFact(_HarnDataclass):
    state: HarnSessionRecapCompletionState; finalStatus: Optional[str]; stopReason: Optional[str]
    kind: Optional[str]; owner: Optional[str]; reason: Optional[str]; sourceEventId: int
@dataclass
class HarnSessionRecapIteration(_HarnDataclass):
    iteration: Optional[int]; state: HarnSessionRecapCompletionState; assistantText: List[HarnSessionRecapTextFact]
    tools: List[HarnSessionRecapToolExchange]; plans: List[HarnSessionRecapPlanFact]
    progress: List[HarnSessionRecapProgressFact]; sourceEventIds: List[int]
@dataclass
class HarnSessionPromptTurnRecap(_HarnDataclass):
    turnId: str; runId: str; state: HarnSessionRecapCompletionState; prompts: List[HarnSessionRecapTextFact]
    iterations: List[HarnSessionRecapIteration]; terminal: Optional[HarnSessionRecapTerminalFact]; sourceEventIds: List[int]
@dataclass
class HarnSessionRecapSnapshot(_HarnDataclass):
    schemaVersion: int; sessionId: str; query: HarnSessionRecapQuery; cursor: HarnSessionRecapCursor
    coverage: HarnSessionRecapCoverage; source: HarnSessionRecapSource; contentHash: str; projectionHash: str
    turns: List[HarnSessionPromptTurnRecap]; extensions: Dict[str, JsonValue]
@dataclass
class HarnSessionRecapAvailability(_HarnDataclass):
    state: str; snapshot: Optional[HarnSessionRecapSnapshot] = None; reason: Optional[HarnSessionRecapUnavailableReason] = None
"#,
    );
}

pub(super) fn append_go_session_recap_types(out: &mut String) {
    out.push_str(
        r#"

const HarnSessionRecapQueryMethod = "harn.session_recap.query"
const HarnSessionRecapSchemaVersion uint32 = 1

type HarnSessionRecapCompletionState string
type HarnSessionRecapToolState string
type HarnSessionRecapPlanStepStatus string
type HarnSessionRecapPlanEventKind string
type HarnSessionRecapProgressStatus string
type HarnSessionRecapProgressPriority string
type HarnSessionRecapUnavailableReason string

type HarnSessionRecapQuery struct {
	SessionID string `json:"sessionId"`; RunID *string `json:"runId"`; TurnID *string `json:"turnId"`; FromEventID *uint64 `json:"fromEventId"`; Limit *uint64 `json:"limit"`
}
type HarnSessionRecapCursor struct { LastEventID *uint64 `json:"lastEventId"`; NextEventID *uint64 `json:"nextEventId"` }
type HarnSessionRecapCoverage struct { Scanned uint64 `json:"scanned"`; Matched uint64 `json:"matched"`; Pending uint64 `json:"pending"`; Unassigned uint64 `json:"unassigned"`; Truncated bool `json:"truncated"` }
type HarnSessionRecapSourceEvent struct { EventID uint64 `json:"eventId"`; RecordHash string `json:"recordHash"` }
type HarnSessionRecapSource struct { FirstEventID *uint64 `json:"firstEventId"`; LastEventID *uint64 `json:"lastEventId"`; Events []HarnSessionRecapSourceEvent `json:"events"` }
type HarnSessionRecapTextFact struct { Text string `json:"text"`; SourceEventID uint64 `json:"sourceEventId"` }
type HarnSessionRecapVerificationFact struct { Schema string `json:"schema"`; Status string `json:"status"`; VerifiedPaths []string `json:"verifiedPaths"`; SourceEventID uint64 `json:"sourceEventId"` }
type HarnSessionRecapToolExchange struct {
	ToolCallID string `json:"toolCallId"`; ToolName *string `json:"toolName"`; State HarnSessionRecapToolState `json:"state"`
	CallObserved bool `json:"callObserved"`; ResultObserved bool `json:"resultObserved"`; Input JSONValue `json:"input"`; Output JSONValue `json:"output"`
	Verification *HarnSessionRecapVerificationFact `json:"verification"`; SourceEventIDs []uint64 `json:"sourceEventIds"`
}
type HarnSessionRecapPlanStep struct { ID string `json:"id"`; Content string `json:"content"`; Status HarnSessionRecapPlanStepStatus `json:"status"` }
type HarnSessionRecapPlanEventFact struct { Kind HarnSessionRecapPlanEventKind `json:"kind"`; EventID string `json:"eventId"`; InputRevisionID *string `json:"inputRevisionId"` }
type HarnSessionRecapPlanFact struct {
	DocumentID string `json:"documentId"`; RevisionID string `json:"revisionId"`; Title string `json:"title"`; Summary string `json:"summary"`
	Steps []HarnSessionRecapPlanStep `json:"steps"`; Event *HarnSessionRecapPlanEventFact `json:"event"`; SourceEventID uint64 `json:"sourceEventId"`
}
type HarnSessionRecapProgressEntry struct { Content string `json:"content"`; Status HarnSessionRecapProgressStatus `json:"status"`; Priority *HarnSessionRecapProgressPriority `json:"priority"` }
type HarnSessionRecapProgressFact struct { Message *string `json:"message"`; Entries []HarnSessionRecapProgressEntry `json:"entries"`; Replace bool `json:"replace"`; SourceEventID uint64 `json:"sourceEventId"` }
type HarnSessionRecapTerminalFact struct {
	State HarnSessionRecapCompletionState `json:"state"`; FinalStatus *string `json:"finalStatus"`; StopReason *string `json:"stopReason"`
	Kind *string `json:"kind"`; Owner *string `json:"owner"`; Reason *string `json:"reason"`; SourceEventID uint64 `json:"sourceEventId"`
}
type HarnSessionRecapIteration struct {
	Iteration *int64 `json:"iteration"`; State HarnSessionRecapCompletionState `json:"state"`; AssistantText []HarnSessionRecapTextFact `json:"assistantText"`
	Tools []HarnSessionRecapToolExchange `json:"tools"`; Plans []HarnSessionRecapPlanFact `json:"plans"`; Progress []HarnSessionRecapProgressFact `json:"progress"`; SourceEventIDs []uint64 `json:"sourceEventIds"`
}
type HarnSessionPromptTurnRecap struct {
	TurnID string `json:"turnId"`; RunID string `json:"runId"`; State HarnSessionRecapCompletionState `json:"state"`; Prompts []HarnSessionRecapTextFact `json:"prompts"`
	Iterations []HarnSessionRecapIteration `json:"iterations"`; Terminal *HarnSessionRecapTerminalFact `json:"terminal"`; SourceEventIDs []uint64 `json:"sourceEventIds"`
}
type HarnSessionRecapSnapshot struct {
	SchemaVersion uint32 `json:"schemaVersion"`; SessionID string `json:"sessionId"`; Query HarnSessionRecapQuery `json:"query"`
	Cursor HarnSessionRecapCursor `json:"cursor"`; Coverage HarnSessionRecapCoverage `json:"coverage"`; Source HarnSessionRecapSource `json:"source"`
	ContentHash string `json:"contentHash"`; ProjectionHash string `json:"projectionHash"`; Turns []HarnSessionPromptTurnRecap `json:"turns"`; Extensions JSONObject `json:"extensions"`
}
type HarnSessionRecapAvailability struct { State string `json:"state"`; Snapshot *HarnSessionRecapSnapshot `json:"snapshot,omitempty"`; Reason *HarnSessionRecapUnavailableReason `json:"reason,omitempty"` }
"#,
    );
}
