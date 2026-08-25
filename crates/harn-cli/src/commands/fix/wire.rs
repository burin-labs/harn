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
    /// Summary of the parameter-annotation migration: how many parameters got
    /// an inferred type and how many were left as `unknown`. Omitted when the run
    /// planned no annotations, so existing consumers see byte-identical
    /// output.
    #[serde(
        rename = "parameterAnnotations",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub parameter_annotations: Option<ParameterAnnotationsWire>,
    pub diagnostics: Vec<DiagnosticWire>,
    pub repairs: Vec<RepairWire>,
    #[serde(rename = "skippedFiles")]
    pub skipped_files: Vec<SkippedFileWire>,
    /// Files the repository declares are expected to be unparseable (a sibling
    /// `.error` fixture). Excluded from repair like any unparseable file, but
    /// they do not fail the run. Omitted when empty so existing consumers see
    /// byte-identical output.
    #[serde(
        rename = "declaredInvalidFiles",
        default,
        skip_serializing_if = "Vec::is_empty"
    )]
    pub declared_invalid_files: Vec<SkippedFileWire>,
    #[serde(rename = "safetyLevels")]
    pub safety_levels: Vec<String>,
    /// Callables the capability migration wanted to re-sign but could not.
    /// Omitted when empty so existing consumers see byte-identical output.
    #[serde(
        rename = "frozenCallables",
        default,
        skip_serializing_if = "Vec::is_empty"
    )]
    pub frozen_callables: Vec<FrozenCallableWire>,
}

/// One callable whose signature the migration froze, and why.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct FrozenCallableWire {
    pub name: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ApplyResult {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    pub applied: Vec<AppliedRepairWire>,
    pub skipped: Vec<SkippedRepairWire>,
    #[serde(rename = "skippedFiles")]
    pub skipped_files: Vec<SkippedFileWire>,
    /// See [`RepairPlan::declared_invalid_files`].
    #[serde(
        rename = "declaredInvalidFiles",
        default,
        skip_serializing_if = "Vec::is_empty"
    )]
    pub declared_invalid_files: Vec<SkippedFileWire>,
    #[serde(rename = "post_apply_diagnostics_count")]
    pub post_apply_diagnostics_count: usize,
    /// See [`RepairPlan::parameter_annotations`].
    #[serde(
        rename = "parameterAnnotations",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub parameter_annotations: Option<ParameterAnnotationsWire>,
    #[serde(rename = "dryRun")]
    pub dry_run: bool,
    /// Callables the migration froze; see [`RepairPlan::frozen_callables`].
    #[serde(
        rename = "frozenCallables",
        default,
        skip_serializing_if = "Vec::is_empty"
    )]
    pub frozen_callables: Vec<FrozenCallableWire>,
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

/// What the parameter-annotation migration inferred and what it gave up on.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ParameterAnnotationsWire {
    pub total: usize,
    pub inferred: usize,
    /// Parameters written as `unknown` because nothing proved a type.
    pub unresolved: usize,
    /// `unresolved / total`, rounded to four decimal places.
    #[serde(rename = "unresolvedShare")]
    pub unresolved_share: f64,
    /// Count per reason, for both outcomes.
    pub causes: std::collections::BTreeMap<String, usize>,
}
