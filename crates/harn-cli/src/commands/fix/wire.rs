use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FixRunError {
    Command(String),
    PartialFailure(String),
}

impl FixRunError {
    pub(crate) fn message(&self) -> &str {
        match self {
            Self::Command(message) | Self::PartialFailure(message) => message,
        }
    }

    pub(crate) fn is_partial_failure(&self) -> bool {
        matches!(self, Self::PartialFailure(_))
    }
}

impl From<String> for FixRunError {
    fn from(message: String) -> Self {
        Self::Command(message)
    }
}

impl std::fmt::Display for FixRunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for FixRunError {}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct RepairPlan {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    pub path: String,
    pub diagnostics: Vec<DiagnosticWire>,
    pub repairs: Vec<RepairWire>,
    #[serde(rename = "skippedFiles")]
    pub skipped_files: Vec<SkippedFileWire>,
    #[serde(rename = "safetyLevels")]
    pub safety_levels: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ApplyResult {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    pub applied: Vec<AppliedRepairWire>,
    pub skipped: Vec<SkippedRepairWire>,
    #[serde(rename = "skippedFiles")]
    pub skipped_files: Vec<SkippedFileWire>,
    #[serde(rename = "post_apply_diagnostics_count")]
    pub post_apply_diagnostics_count: usize,
    #[serde(rename = "dryRun")]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct AppliedRepairWire {
    pub diagnostic_code: String,
    pub repair_id: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SkippedRepairWire {
    pub diagnostic_index: usize,
    pub diagnostic_code: String,
    pub repair_id: String,
    pub path: String,
    pub reason: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SkippedFileWire {
    pub path: String,
    pub reason: &'static str,
    pub diagnostics: Vec<SkippedFileDiagnosticWire>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SkippedFileDiagnosticWire {
    pub source: &'static str,
    pub severity: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span: Option<SpanWire>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct DiagnosticWire {
    pub index: usize,
    pub file: String,
    pub source: &'static str,
    pub severity: &'static str,
    pub code: String,
    pub message: String,
    pub span: Option<SpanWire>,
    pub repair: RepairMetadataWire,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct RepairWire {
    pub diagnostic_index: usize,
    pub diagnostic_code: String,
    pub repair: RepairMetadataWire,
    pub impact: RepairImpactWire,
    pub edits: Vec<FixEditWire>,
    pub applies_cleanly: bool,
    pub conflicts_with: Vec<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct RepairImpactWire {
    pub classification: String,
    pub strategy: Option<String>,
    #[serde(rename = "signatureChanges")]
    pub signature_changes: Vec<SignatureChangeWire>,
    #[serde(rename = "requiresCrossModuleCallerUpdates")]
    pub requires_cross_module_caller_updates: bool,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct SignatureChangeWire {
    pub callable: String,
    #[serde(rename = "isExported")]
    pub is_exported: bool,
    #[serde(rename = "isEntrypoint")]
    pub is_entrypoint: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct RepairMetadataWire {
    pub id: String,
    pub summary: String,
    pub safety: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct FixEditWire {
    pub span: SpanWire,
    pub replacement: String,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub(crate) struct SpanWire {
    pub start: usize,
    pub end: usize,
    pub line: usize,
    pub column: usize,
    pub end_line: usize,
}
