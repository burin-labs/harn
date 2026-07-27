//! Public JSON Schema and fail-closed decoder for `harn lint --json`.
//!
//! The schema is published through `harn --json-schemas --command lint`.
//! Continuous verification against the Rust wire types lives in the unit
//! tests and fixture matrix shared with `std/cli/envelope`.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::json_envelope::{JsonError, JsonWarning};

use super::lint_report::LINT_SCHEMA_VERSION;

/// Options for [`decode_lint_envelope`] / [`decode_lint_json`].
#[derive(Debug, Clone, Copy, Default)]
pub struct LintDecodeOptions {
    /// When set, require `(exit_status == 0) == envelope.ok`.
    pub exit_status: Option<i32>,
    /// Defaults to [`LINT_SCHEMA_VERSION`] when `None`.
    pub expected_schema_version: Option<u32>,
}

/// Successfully decoded schema-v1 lint envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecodedLintEnvelope {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    pub ok: bool,
    pub data: Option<LintReportWire>,
    pub error: Option<JsonError>,
    #[serde(default)]
    pub warnings: Vec<JsonWarning>,
}

/// Wire shape of `JsonEnvelope.data` for `harn lint --json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LintReportWire {
    pub files: Vec<LintFileReportWire>,
    pub summary: LintSummaryWire,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changed: Option<ChangedLintScopeWire>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LintFileReportWire {
    pub path: String,
    pub status: String,
    pub diagnostics: Vec<CheckDiagnosticWire>,
    pub fixable: u64,
    pub fixed: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LintSummaryWire {
    pub ok: u64,
    pub warnings: u64,
    pub errors: u64,
    pub diagnostics: u64,
    pub fixable: u64,
    pub fixed: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckDiagnosticWire {
    pub source: String,
    pub severity: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span: Option<CheckSpanWire>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
}

/// UTF-8 half-open byte span `[start, end)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckSpanWire {
    pub start: u64,
    pub end: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChangedLintScopeWire {
    pub from: EvaluatedRevisionWire,
    pub to: EvaluatedRevisionWire,
    pub files: Vec<ChangedSourceFileWire>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvaluatedRevisionWire {
    pub requested: String,
    pub commit: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChangedSourceFileWire {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_path: Option<String>,
    pub status: String,
    pub added_lines: Vec<AddedLineRangeWire>,
}

/// Inclusive one-based physical line range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddedLineRangeWire {
    pub start: u64,
    pub end: u64,
}

/// Fail-closed decode error for lint JSON envelopes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LintDecodeError {
    pub kind: &'static str,
    pub message: String,
}

impl std::fmt::Display for LintDecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.kind, self.message)
    }
}

impl std::error::Error for LintDecodeError {}

impl LintDecodeError {
    fn new(kind: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

/// Complete Draft 2020-12 JSON Schema for the schema-v1 lint envelope.
pub fn lint_json_schema() -> Value {
    let non_neg_int = json!({ "type": "integer", "minimum": 0 });
    let span = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["start", "end"],
        "properties": {
            "start": non_neg_int.clone(),
            "end": non_neg_int.clone(),
        },
        "description": "UTF-8 half-open byte span [start, end)."
    });
    let diagnostic = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["source", "severity", "message"],
        "properties": {
            "source": { "type": "string", "minLength": 1 },
            "severity": { "type": "string", "enum": ["info", "warning", "error"] },
            "code": { "type": "string", "minLength": 1 },
            "message": { "type": "string" },
            "span": span,
            "help": { "type": "string" },
        }
    });
    let file_report = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["path", "status", "diagnostics", "fixable", "fixed"],
        "properties": {
            "path": { "type": "string", "minLength": 1 },
            "status": { "type": "string", "enum": ["ok", "warning", "error"] },
            "diagnostics": { "type": "array", "items": diagnostic },
            "fixable": non_neg_int.clone(),
            "fixed": non_neg_int.clone(),
        }
    });
    let summary = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["ok", "warnings", "errors", "diagnostics", "fixable", "fixed"],
        "properties": {
            "ok": non_neg_int.clone(),
            "warnings": non_neg_int.clone(),
            "errors": non_neg_int.clone(),
            "diagnostics": non_neg_int.clone(),
            "fixable": non_neg_int.clone(),
            "fixed": non_neg_int.clone(),
        }
    });
    let added_line = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["start", "end"],
        "properties": {
            "start": { "type": "integer", "minimum": 1 },
            "end": { "type": "integer", "minimum": 1 },
        },
        "description": "Inclusive one-based physical line range."
    });
    let changed_file = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["path", "status", "added_lines"],
        "properties": {
            "path": { "type": "string", "minLength": 1 },
            "previous_path": { "type": "string", "minLength": 1 },
            "status": {
                "type": "string",
                "enum": ["added", "copied", "deleted", "modified", "renamed"]
            },
            "added_lines": { "type": "array", "items": added_line },
        }
    });
    let revision = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["requested", "commit"],
        "properties": {
            "requested": { "type": "string", "minLength": 1 },
            "commit": { "type": "string", "minLength": 1 },
        }
    });
    let changed = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["from", "to", "files"],
        "properties": {
            "from": revision.clone(),
            "to": revision,
            "files": { "type": "array", "items": changed_file },
        }
    });
    let report = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["files", "summary"],
        "properties": {
            "files": { "type": "array", "items": file_report },
            "summary": summary,
            "changed": changed,
        }
    });
    let warning = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["code", "message"],
        "properties": {
            "code": { "type": "string", "minLength": 1 },
            "message": { "type": "string" },
        }
    });
    let error = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["code", "message", "details"],
        "properties": {
            "code": { "type": "string", "minLength": 1 },
            "message": { "type": "string", "minLength": 1 },
            "details": {},
        }
    });

    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "harn lint --json",
        "type": "object",
        "additionalProperties": false,
        "required": ["schemaVersion", "ok", "data", "error", "warnings"],
        "properties": {
            "schemaVersion": { "const": LINT_SCHEMA_VERSION },
            "ok": { "type": "boolean" },
            "data": {
                "anyOf": [
                    report,
                    { "type": "null" }
                ]
            },
            "error": {
                "anyOf": [
                    error,
                    { "type": "null" }
                ]
            },
            "warnings": { "type": "array", "items": warning },
        },
        "description": "schema-v1 lint envelope. Diagnostic spans are UTF-8 half-open byte offsets [start, end)."
    })
}

/// Parse JSON text and fail closed on structural or semantic contract violations.
pub fn decode_lint_json(
    text: &str,
    options: LintDecodeOptions,
) -> Result<DecodedLintEnvelope, LintDecodeError> {
    let value: Value = serde_json::from_str(text)
        .map_err(|err| LintDecodeError::new("json_parse", format!("malformed JSON: {err}")))?;
    decode_lint_envelope(&value, options)
}

/// Decode a lint envelope value and fail closed on contract violations.
pub fn decode_lint_envelope(
    value: &Value,
    options: LintDecodeOptions,
) -> Result<DecodedLintEnvelope, LintDecodeError> {
    let expected = options
        .expected_schema_version
        .unwrap_or(LINT_SCHEMA_VERSION);
    let schema_version = value
        .get("schemaVersion")
        .and_then(Value::as_u64)
        .ok_or_else(|| LintDecodeError::new("schema", "missing or non-integer schemaVersion"))?;
    if schema_version != u64::from(expected) {
        return Err(LintDecodeError::new(
            "unsupported_schema_version",
            format!("unsupported schemaVersion {schema_version}; expected {expected}"),
        ));
    }

    let envelope: DecodedLintEnvelope = serde_json::from_value(value.clone()).map_err(|err| {
        LintDecodeError::new(
            "schema",
            format!("envelope does not match wire types: {err}"),
        )
    })?;

    validate_envelope_invariants(&envelope)?;
    if let Some(report) = &envelope.data {
        validate_report(report)?;
    }
    if let Some(exit_status) = options.exit_status {
        let exit_ok = exit_status == 0;
        if exit_ok != envelope.ok {
            return Err(LintDecodeError::new(
                "exit_status_mismatch",
                format!(
                    "process exit status {exit_status} disagrees with envelope.ok={}",
                    envelope.ok
                ),
            ));
        }
    }
    Ok(envelope)
}

fn validate_envelope_invariants(envelope: &DecodedLintEnvelope) -> Result<(), LintDecodeError> {
    if envelope.ok {
        if envelope.error.is_some() {
            return Err(LintDecodeError::new(
                "envelope_invariant",
                "ok=true requires error=null",
            ));
        }
        if envelope.data.is_none() {
            return Err(LintDecodeError::new(
                "envelope_invariant",
                "ok=true requires a lint report in data",
            ));
        }
    } else {
        let error = envelope.error.as_ref().ok_or_else(|| {
            LintDecodeError::new("envelope_invariant", "ok=false requires an error object")
        })?;
        if error.code.is_empty() || error.message.is_empty() {
            return Err(LintDecodeError::new(
                "envelope_invariant",
                "error.code and error.message must be non-empty",
            ));
        }
    }
    Ok(())
}

fn validate_report(report: &LintReportWire) -> Result<(), LintDecodeError> {
    let mut ok = 0u64;
    let mut warnings = 0u64;
    let mut errors = 0u64;
    let mut diagnostics = 0u64;
    let mut fixable = 0u64;
    let mut fixed = 0u64;

    for (index, file) in report.files.iter().enumerate() {
        validate_file(file, index)?;
        match file.status.as_str() {
            "ok" => ok += 1,
            "warning" => warnings += 1,
            "error" => errors += 1,
            other => {
                return Err(LintDecodeError::new(
                    "invalid_status",
                    format!("files[{index}].status has unsupported value {other:?}"),
                ));
            }
        }
        diagnostics += file.diagnostics.len() as u64;
        fixable += file.fixable;
        fixed += file.fixed;
    }

    let summary = &report.summary;
    for (name, expected, actual) in [
        ("ok", ok, summary.ok),
        ("warnings", warnings, summary.warnings),
        ("errors", errors, summary.errors),
        ("diagnostics", diagnostics, summary.diagnostics),
        ("fixable", fixable, summary.fixable),
        ("fixed", fixed, summary.fixed),
    ] {
        if expected != actual {
            return Err(LintDecodeError::new(
                "inconsistent_aggregate",
                format!("summary.{name}={actual} disagrees with file-derived count {expected}"),
            ));
        }
    }

    if let Some(changed) = &report.changed {
        validate_changed(changed)?;
    }
    Ok(())
}

fn validate_file(file: &LintFileReportWire, index: usize) -> Result<(), LintDecodeError> {
    if file.path.is_empty() {
        return Err(LintDecodeError::new(
            "schema",
            format!("files[{index}].path must be non-empty"),
        ));
    }

    let mut has_error = false;
    let mut has_warning = false;
    for (diag_index, diagnostic) in file.diagnostics.iter().enumerate() {
        match diagnostic.severity.as_str() {
            "error" => has_error = true,
            "warning" => has_warning = true,
            "info" => {}
            other => {
                return Err(LintDecodeError::new(
                    "invalid_severity",
                    format!(
                        "files[{index}].diagnostics[{diag_index}].severity has unsupported value {other:?}"
                    ),
                ));
            }
        }
        if diagnostic.source.is_empty() {
            return Err(LintDecodeError::new(
                "schema",
                format!("files[{index}].diagnostics[{diag_index}].source must be non-empty"),
            ));
        }
        if let Some(span) = diagnostic.span {
            if span.start > span.end {
                return Err(LintDecodeError::new(
                    "invalid_span",
                    format!(
                        "files[{index}].diagnostics[{diag_index}].span has start {} > end {}",
                        span.start, span.end
                    ),
                ));
            }
        }
    }

    let expected_status = if has_error {
        "error"
    } else if has_warning {
        "warning"
    } else {
        "ok"
    };
    if file.status != expected_status {
        return Err(LintDecodeError::new(
            "inconsistent_status",
            format!(
                "files[{index}].status={:?} disagrees with diagnostics (expected {expected_status:?})",
                file.status
            ),
        ));
    }
    Ok(())
}

fn validate_changed(changed: &ChangedLintScopeWire) -> Result<(), LintDecodeError> {
    for field in [
        ("from.requested", changed.from.requested.as_str()),
        ("from.commit", changed.from.commit.as_str()),
        ("to.requested", changed.to.requested.as_str()),
        ("to.commit", changed.to.commit.as_str()),
    ] {
        if field.1.is_empty() {
            return Err(LintDecodeError::new(
                "schema",
                format!("changed.{} must be non-empty", field.0),
            ));
        }
    }
    for (index, file) in changed.files.iter().enumerate() {
        match file.status.as_str() {
            "added" | "copied" | "deleted" | "modified" | "renamed" => {}
            other => {
                return Err(LintDecodeError::new(
                    "schema",
                    format!("changed.files[{index}].status has unsupported value {other:?}"),
                ));
            }
        }
        for (range_index, range) in file.added_lines.iter().enumerate() {
            if range.start == 0 || range.end == 0 || range.start > range.end {
                return Err(LintDecodeError::new(
                    "invalid_span",
                    format!(
                        "changed.files[{index}].added_lines[{range_index}] must be inclusive 1-based with start <= end"
                    ),
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn options_with_exit(exit_status: i32) -> LintDecodeOptions {
        LintDecodeOptions {
            exit_status: Some(exit_status),
            ..LintDecodeOptions::default()
        }
    }

    #[test]
    fn schema_is_draft_2020_12_and_accepts_live_shapes() {
        let schema = lint_json_schema();
        jsonschema::draft202012::meta::validate(&schema).expect("meta-schema");
        let validator = jsonschema::draft202012::new(&schema).expect("compile schema");

        let ok = json!({
            "schemaVersion": 1,
            "ok": true,
            "data": {
                "files": [{
                    "path": "src/ok.harn",
                    "status": "ok",
                    "diagnostics": [],
                    "fixable": 0,
                    "fixed": 0
                }],
                "summary": {
                    "ok": 1,
                    "warnings": 0,
                    "errors": 0,
                    "diagnostics": 0,
                    "fixable": 0,
                    "fixed": 0
                }
            },
            "error": null,
            "warnings": []
        });
        validator.validate(&ok).expect("ok envelope validates");

        let failed = json!({
            "schemaVersion": 1,
            "ok": false,
            "data": {
                "files": [{
                    "path": "src/agent.harn",
                    "status": "warning",
                    "diagnostics": [{
                        "source": "lint",
                        "severity": "warning",
                        "code": "HARN-LNT-032",
                        "message": "comparison to `false` is redundant",
                        "span": { "start": 128, "end": 142 }
                    }],
                    "fixable": 1,
                    "fixed": 0
                }],
                "summary": {
                    "ok": 0,
                    "warnings": 1,
                    "errors": 0,
                    "diagnostics": 1,
                    "fixable": 1,
                    "fixed": 0
                }
            },
            "error": {
                "code": "lint_failed",
                "message": "one or more files failed `harn lint`",
                "details": null
            },
            "warnings": []
        });
        validator
            .validate(&failed)
            .expect("lint_failed envelope validates");
    }

    #[test]
    fn decoder_accepts_positive_and_rejects_adversarial() {
        let positive = include_str!("../../../tests/fixtures/lint_json/positive/clean_ok.json");
        decode_lint_json(positive, options_with_exit(0)).expect("clean ok");

        let warning = include_str!("../../../tests/fixtures/lint_json/positive/warning_ok.json");
        decode_lint_json(warning, options_with_exit(0)).expect("warning ok");

        let failed =
            include_str!("../../../tests/fixtures/lint_json/positive/lint_failed_with_data.json");
        decode_lint_json(failed, options_with_exit(1)).expect("lint_failed");

        let utf8 =
            include_str!("../../../tests/fixtures/lint_json/positive/multiline_utf8_span.json");
        let decoded = decode_lint_json(utf8, options_with_exit(0)).expect("utf8 span");
        let span = decoded.data.as_ref().unwrap().files[0].diagnostics[0]
            .span
            .expect("span");
        assert_eq!(span.start, 11);
        assert_eq!(span.end, 24);

        let changed = include_str!("../../../tests/fixtures/lint_json/positive/changed_scope.json");
        decode_lint_json(changed, options_with_exit(0)).expect("changed scope");

        assert_eq!(
            decode_lint_json("{not json", LintDecodeOptions::default())
                .unwrap_err()
                .kind,
            "json_parse"
        );
        assert_eq!(
            decode_lint_json(
                include_str!(
                    "../../../tests/fixtures/lint_json/adversarial/unsupported_schema_version.json"
                ),
                LintDecodeOptions::default()
            )
            .unwrap_err()
            .kind,
            "unsupported_schema_version"
        );
        assert_eq!(
            decode_lint_json(
                include_str!("../../../tests/fixtures/lint_json/adversarial/invalid_severity.json"),
                LintDecodeOptions::default()
            )
            .unwrap_err()
            .kind,
            "invalid_severity"
        );
        assert_eq!(
            decode_lint_json(
                include_str!("../../../tests/fixtures/lint_json/adversarial/invalid_span.json"),
                LintDecodeOptions::default()
            )
            .unwrap_err()
            .kind,
            "invalid_span"
        );
        assert_eq!(
            decode_lint_json(
                include_str!(
                    "../../../tests/fixtures/lint_json/adversarial/inconsistent_aggregate.json"
                ),
                LintDecodeOptions::default()
            )
            .unwrap_err()
            .kind,
            "inconsistent_aggregate"
        );
        assert_eq!(
            decode_lint_json(
                include_str!(
                    "../../../tests/fixtures/lint_json/adversarial/inconsistent_status.json"
                ),
                LintDecodeOptions::default()
            )
            .unwrap_err()
            .kind,
            "inconsistent_status"
        );
        assert_eq!(
            decode_lint_json(
                include_str!(
                    "../../../tests/fixtures/lint_json/adversarial/exit_status_mismatch.json"
                ),
                options_with_exit(1)
            )
            .unwrap_err()
            .kind,
            "exit_status_mismatch"
        );
    }
}
