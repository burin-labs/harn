use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use super::{
    AuthorityDecider, AuthorityDiagnostic, AuthorityRequirement, RUN_AUTHORITY_RECEIPT_SCHEMA,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorityReceiptStage {
    Startup,
    NeedsApproval,
    Blocked,
    Ready,
    Terminal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorityReceiptStatus {
    Preparing,
    NeedsApproval,
    Blocked,
    Ready,
    Completed,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReceiptedAuthority {
    pub fingerprint: String,
    pub requirement: AuthorityRequirement,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DeniedAuthority {
    pub authority: ReceiptedAuthority,
    pub reason: String,
    pub decider: AuthorityDecider,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PolicyDecisionEvidence {
    pub requirement_fingerprint: String,
    pub action: String,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_rule_id: Option<String>,
    pub risk_labels: Vec<String>,
    /// The canonical `policyDecision` receipt produced by the permission
    /// evaluator. Hosts project this value; they do not reconstruct it.
    pub policy_decision: serde_json::Value,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RunAuthorityReceipt {
    pub schema: String,
    pub stage: AuthorityReceiptStage,
    pub status: AuthorityReceiptStatus,
    pub intent_id: String,
    pub plan_fingerprint: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lease_fingerprint: Option<String>,
    pub observed_at_ms: u64,
    pub requested: Vec<ReceiptedAuthority>,
    pub granted: Vec<ReceiptedAuthority>,
    pub used: Vec<ReceiptedAuthority>,
    pub denied: Vec<DeniedAuthority>,
    pub unused: Vec<ReceiptedAuthority>,
    pub deciders: BTreeMap<String, AuthorityDecider>,
    pub policy_decisions: Vec<PolicyDecisionEvidence>,
    pub diagnostics: Vec<AuthorityDiagnostic>,
    pub executor_invoked: bool,
}

impl RunAuthorityReceipt {
    pub(crate) fn startup(
        intent_id: String,
        plan_fingerprint: String,
        requested: Vec<ReceiptedAuthority>,
        observed_at_ms: u64,
    ) -> Self {
        Self {
            schema: RUN_AUTHORITY_RECEIPT_SCHEMA.to_string(),
            stage: AuthorityReceiptStage::Startup,
            status: AuthorityReceiptStatus::Preparing,
            intent_id,
            plan_fingerprint,
            lease_fingerprint: None,
            observed_at_ms,
            requested,
            granted: Vec::new(),
            used: Vec::new(),
            denied: Vec::new(),
            unused: Vec::new(),
            deciders: BTreeMap::new(),
            policy_decisions: Vec::new(),
            diagnostics: Vec::new(),
            executor_invoked: false,
        }
    }
}

pub trait AuthorityReceiptSink: Send + Sync {
    /// Return the stable location this sink persists to when it has one.
    /// PreparedRun rejects a declared URI that names a different location.
    fn persistent_uri(&self) -> Option<String> {
        None
    }

    /// Persist one immutable receipt event. Implementations must return only
    /// after the event is durable enough for a subsequent startup phase to
    /// rely on it.
    fn persist(&self, receipt: &RunAuthorityReceipt) -> Result<(), String>;
}

#[derive(Debug)]
pub struct NdjsonAuthorityReceiptSink {
    path: PathBuf,
    writer: Mutex<()>,
}

impl NdjsonAuthorityReceiptSink {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            writer: Mutex::new(()),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl AuthorityReceiptSink for NdjsonAuthorityReceiptSink {
    fn persistent_uri(&self) -> Option<String> {
        Some(self.path.to_string_lossy().into_owned())
    }

    fn persist(&self, receipt: &RunAuthorityReceipt) -> Result<(), String> {
        let _guard = self
            .writer
            .lock()
            .map_err(|_| "authority receipt writer lock is poisoned".to_string())?;
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                format!(
                    "create authority receipt directory '{}': {error}",
                    parent.display()
                )
            })?;
        }
        let mut line = crate::canonical_json::of(receipt)
            .map_err(|error| format!("encode authority receipt: {error}"))?;
        line.push('\n');
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|error| {
                format!("open authority receipt '{}': {error}", self.path.display())
            })?;
        file.write_all(line.as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(|error| {
                format!(
                    "persist authority receipt '{}': {error}",
                    self.path.display()
                )
            })
    }
}

#[derive(Debug, Default)]
pub struct MemoryAuthorityReceiptSink {
    receipts: Mutex<Vec<RunAuthorityReceipt>>,
}

impl MemoryAuthorityReceiptSink {
    pub fn receipts(&self) -> Vec<RunAuthorityReceipt> {
        self.receipts
            .lock()
            .expect("memory authority receipt sink poisoned")
            .clone()
    }
}

impl AuthorityReceiptSink for MemoryAuthorityReceiptSink {
    fn persist(&self, receipt: &RunAuthorityReceipt) -> Result<(), String> {
        self.receipts
            .lock()
            .map_err(|_| "memory authority receipt sink poisoned".to_string())?
            .push(receipt.clone());
        Ok(())
    }
}
