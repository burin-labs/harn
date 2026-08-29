//! Generated host bindings for the Harn-owned prepared-session protocol.

pub(super) const PREPARED_SESSION_STATES: &[&str] = &[
    "needs_approval",
    "ready",
    "blocked",
    "active",
    "delta",
    "stopped",
    "pivoted",
    "terminal",
];

pub(super) const PREPARED_SESSION_COMMANDS: &[&str] = &[
    "approval_decision",
    "attach",
    "turn",
    "request_delta",
    "stop",
    "pivot",
    "finish",
];

pub(super) fn append_typescript_prepared_session_types(out: &mut String) {
    out.push_str(
        r#"
export const HARN_PREPARED_SESSION_SCHEMA = "harn.prepared_session.v1" as const
export type HarnPreparedSessionState = "needs_approval" | "ready" | "blocked" | "active" | "delta" | "stopped" | "pivoted" | "terminal"
export type HarnPreparedSessionCommand = "approval_decision" | "attach" | "turn" | "request_delta" | "stop" | "pivot" | "finish"
export interface HarnPreparedSessionApprovalDecision { batch_fingerprint: string; approved: boolean; decider: string }
export interface HarnPreparedSessionBinding { session_id: string; workspace_fingerprint: string; runtime: ACPValue; consumer: ACPValue }
export interface HarnPreparedRuntimeAttachment { session_id: string; workspace_fingerprint: string; runtime: ACPValue; consumer: ACPValue }
export interface HarnPreparedSessionLease {
  schema: typeof HARN_PREPARED_SESSION_SCHEMA; session_id: string; session_fingerprint: string
  plan_fingerprint: string; binding: HarnPreparedSessionBinding; intent: ACPValue
  approval?: HarnPreparedSessionApprovalDecision | null; issued_at_ms: number; expires_at_ms: number
}
export interface HarnPreparedSessionUpdate {
  state: HarnPreparedSessionState; session_id: string; batch?: ACPValue; lease?: HarnPreparedSessionLease
  diagnostics?: ACPValue[]; receipt?: ACPValue; outcome?: ACPValue
}
"#,
    );
}

pub(super) fn append_rust_prepared_session_types(out: &mut String) {
    out.push_str(
        r#"
pub const HARN_PREPARED_SESSION_SCHEMA: &str = "harn.prepared_session.v1";
pub const HARN_PREPARED_SESSION_STATES: &[&str] = &["needs_approval", "ready", "blocked", "active", "delta", "stopped", "pivoted", "terminal"];
pub const HARN_PREPARED_SESSION_COMMANDS: &[&str] = &["approval_decision", "attach", "turn", "request_delta", "stop", "pivot", "finish"];
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnPreparedSessionApprovalDecision { pub batch_fingerprint: String, pub approved: bool, pub decider: String }
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnPreparedSessionBinding { pub session_id: String, pub workspace_fingerprint: String, pub runtime: Value, pub consumer: Value }
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnPreparedRuntimeAttachment { pub session_id: String, pub workspace_fingerprint: String, pub runtime: Value, pub consumer: Value }
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnPreparedSessionLease {
    pub schema: String, pub session_id: String, pub session_fingerprint: String, pub plan_fingerprint: String,
    pub binding: HarnPreparedSessionBinding, pub intent: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub approval: Option<HarnPreparedSessionApprovalDecision>,
    pub issued_at_ms: u64, pub expires_at_ms: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnPreparedSessionUpdate {
    pub state: String, pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub batch: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub lease: Option<HarnPreparedSessionLease>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub diagnostics: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub receipt: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub outcome: Option<Value>,
}
"#,
    );
}

pub(super) fn append_swift_prepared_session_types(out: &mut String) {
    out.push_str(
        r#"
public let harnPreparedSessionSchema = "harn.prepared_session.v1"
public enum HarnPreparedSessionState: String, Codable, Sendable { case needsApproval = "needs_approval", ready, blocked, active, delta, stopped, pivoted, terminal }
public enum HarnPreparedSessionCommand: String, Codable, Sendable { case approvalDecision = "approval_decision", attach, turn, requestDelta = "request_delta", stop, pivot, finish }
public struct HarnPreparedSessionApprovalDecision: Codable, Sendable, Equatable { public var batch_fingerprint: String; public var approved: Bool; public var decider: String }
public struct HarnPreparedSessionBinding: Codable, Sendable, Equatable { public var session_id: String; public var workspace_fingerprint: String; public var runtime: HarnACPValue; public var consumer: HarnACPValue }
public struct HarnPreparedRuntimeAttachment: Codable, Sendable, Equatable { public var session_id: String; public var workspace_fingerprint: String; public var runtime: HarnACPValue; public var consumer: HarnACPValue }
public struct HarnPreparedSessionLease: Codable, Sendable, Equatable {
    public var schema: String; public var session_id: String; public var session_fingerprint: String; public var plan_fingerprint: String
    public var binding: HarnPreparedSessionBinding; public var intent: HarnACPValue; public var approval: HarnPreparedSessionApprovalDecision?
    public var issued_at_ms: Int; public var expires_at_ms: Int
}
public struct HarnPreparedSessionUpdate: Codable, Sendable, Equatable {
    public var state: HarnPreparedSessionState; public var session_id: String; public var batch: HarnACPValue?
    public var lease: HarnPreparedSessionLease?; public var diagnostics: [HarnACPValue]?; public var receipt: HarnACPValue?; public var outcome: HarnACPValue?
}
"#,
    );
}

pub(super) fn append_python_prepared_session_types(out: &mut String) {
    out.push_str(
        r#"
HARN_PREPARED_SESSION_SCHEMA = "harn.prepared_session.v1"
HARN_PREPARED_SESSION_STATES = ("needs_approval", "ready", "blocked", "active", "delta", "stopped", "pivoted", "terminal")
HARN_PREPARED_SESSION_COMMANDS = ("approval_decision", "attach", "turn", "request_delta", "stop", "pivot", "finish")
@dataclass
class HarnPreparedSessionApprovalDecision(_HarnDataclass):
    batch_fingerprint: str
    approved: bool
    decider: str
@dataclass
class HarnPreparedSessionBinding(_HarnDataclass):
    session_id: str
    workspace_fingerprint: str
    runtime: JsonValue
    consumer: JsonValue
@dataclass
class HarnPreparedRuntimeAttachment(_HarnDataclass):
    session_id: str
    workspace_fingerprint: str
    runtime: JsonValue
    consumer: JsonValue
@dataclass
class HarnPreparedSessionLease(_HarnDataclass):
    schema: str
    session_id: str
    session_fingerprint: str
    plan_fingerprint: str
    binding: HarnPreparedSessionBinding
    intent: JsonValue
    issued_at_ms: int
    expires_at_ms: int
    approval: Optional[HarnPreparedSessionApprovalDecision] = None
@dataclass
class HarnPreparedSessionUpdate(_HarnDataclass):
    state: str
    session_id: str
    batch: Optional[JsonValue] = None
    lease: Optional[HarnPreparedSessionLease] = None
    diagnostics: Optional[List[JsonValue]] = None
    receipt: Optional[JsonValue] = None
    outcome: Optional[JsonValue] = None
"#,
    );
}

pub(super) fn append_go_prepared_session_types(out: &mut String) {
    out.push_str(
        r#"
const HarnPreparedSessionSchema = "harn.prepared_session.v1"
var HarnPreparedSessionStates = []string{"needs_approval", "ready", "blocked", "active", "delta", "stopped", "pivoted", "terminal"}
var HarnPreparedSessionCommands = []string{"approval_decision", "attach", "turn", "request_delta", "stop", "pivot", "finish"}
type HarnPreparedSessionApprovalDecision struct { BatchFingerprint string `json:"batch_fingerprint"`; Approved bool `json:"approved"`; Decider string `json:"decider"` }
type HarnPreparedSessionBinding struct { SessionID string `json:"session_id"`; WorkspaceFingerprint string `json:"workspace_fingerprint"`; Runtime json.RawMessage `json:"runtime"`; Consumer json.RawMessage `json:"consumer"` }
type HarnPreparedRuntimeAttachment struct { SessionID string `json:"session_id"`; WorkspaceFingerprint string `json:"workspace_fingerprint"`; Runtime json.RawMessage `json:"runtime"`; Consumer json.RawMessage `json:"consumer"` }
type HarnPreparedSessionLease struct {
    Schema string `json:"schema"`; SessionID string `json:"session_id"`; SessionFingerprint string `json:"session_fingerprint"`; PlanFingerprint string `json:"plan_fingerprint"`
    Binding HarnPreparedSessionBinding `json:"binding"`; Intent json.RawMessage `json:"intent"`; Approval *HarnPreparedSessionApprovalDecision `json:"approval,omitempty"`
    IssuedAtMs int `json:"issued_at_ms"`; ExpiresAtMs int `json:"expires_at_ms"`
}
type HarnPreparedSessionUpdate struct {
    State string `json:"state"`; SessionID string `json:"session_id"`; Batch json.RawMessage `json:"batch,omitempty"`; Lease *HarnPreparedSessionLease `json:"lease,omitempty"`
    Diagnostics []json.RawMessage `json:"diagnostics,omitempty"`; Receipt json.RawMessage `json:"receipt,omitempty"`; Outcome json.RawMessage `json:"outcome,omitempty"`
}
"#,
    );
}
