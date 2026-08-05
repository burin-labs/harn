use super::*;

impl LiveClientMode {
    pub(super) fn as_str(&self) -> &'static str {
        match self {
            Self::Observer => "observer",
            Self::Controller => "controller",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveSessionClient {
    pub client_id: String,
    pub mode: LiveClientMode,
    pub attached_at: String,
    pub last_seen_at: String,
    pub prompt_injection: bool,
    pub permission_routing: bool,
    pub metadata: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachLiveClient {
    pub client_id: String,
    pub mode: LiveClientMode,
    pub takeover: bool,
    pub prompt_injection: bool,
    pub permission_routing: bool,
    pub metadata: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveClientChange {
    pub client: Option<LiveSessionClient>,
    pub previous_controller_id: Option<String>,
    pub active_controller_id: Option<String>,
    pub clients: Vec<LiveSessionClient>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionAncestry {
    pub parent_id: Option<String>,
    pub child_ids: Vec<String>,
    pub root_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionTruncateResult {
    pub kept_turn_count: usize,
    pub removed_turn_count: usize,
    pub new_tip_turn_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionCheckpointSummary {
    pub checkpoint_id: String,
    pub before_message_count: usize,
    pub after_message_count: usize,
    pub fs_snapshot_ids: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct SessionTurnCheckpoint {
    pub checkpoint_id: String,
    pub completed_at: String,
    pub before_transcript: VmValue,
    pub after_transcript: VmValue,
    pub before_message_count: usize,
    pub after_message_count: usize,
    pub fs_snapshot_ids: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct SessionRedoEntry {
    pub checkpoint: SessionTurnCheckpoint,
    pub redo_fs_snapshot_ids: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionCheckpointError {
    UnknownSession,
    NoCheckpoint,
    NoRedo,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionCheckpointOutcome {
    pub status: &'static str,
    pub checkpoint: SessionCheckpointSummary,
    pub redo_fs_snapshot_ids: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReminderInjectionReport {
    pub reminder_id: String,
    pub deduped_count: usize,
}
