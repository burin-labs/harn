//! Declarative artifact emission for agent sessions.
//!
//! `artifact_emit` validates a small safe set of renderable specs, records a
//! transcript event when the session is known locally, and publishes the same
//! payload on the live agent event stream for ACP/A2A surfaces.

use std::collections::BTreeMap;

use serde_json::{json, Value as JsonValue};

use crate::agent_events::AgentEvent;
use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{VmError, VmValue};
use crate::vm::{AsyncBuiltinCtx, Vm};

const DEFAULT_MAX_BYTES: usize = 256 * 1024;
const HARD_MAX_BYTES: usize = 1024 * 1024;
const MAX_FALLBACK_BYTES: usize = 64 * 1024;
const MAX_METADATA_BYTES: usize = 64 * 1024;
const MAX_STRING_BYTES: usize = 64 * 1024;
const MAX_TABLE_COLUMNS: usize = 50;
const MAX_TABLE_ROWS: usize = 500;
const MAX_MERMAID_BYTES: usize = 64 * 1024;

pub fn register_artifact_emit_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[&ARTIFACT_EMIT_BUILTIN_DEF];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ArtifactKind {
    VegaLite,
    Mermaid,
    Table,
}

impl ArtifactKind {
    fn parse(raw: &str) -> Result<Self, VmError> {
        match raw.trim() {
            "vega-lite" => Ok(Self::VegaLite),
            "mermaid" => Ok(Self::Mermaid),
            "table" => Ok(Self::Table),
            other => Err(err(format!(
                "unsupported artifact kind '{other}' (expected one of: vega-lite, mermaid, table)"
            ))),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::VegaLite => "vega-lite",
            Self::Mermaid => "mermaid",
            Self::Table => "table",
        }
    }

    fn mime_type(self) -> &'static str {
        match self {
            Self::VegaLite => "application/vnd.vegalite.v5+json",
            Self::Mermaid => "text/vnd.mermaid",
            Self::Table => "application/vnd.harn.table+json",
        }
    }
}

#[derive(Debug)]
struct ArtifactEmitOptions {
    session_id: String,
    artifact_id: String,
    title: Option<String>,
    fallback: Option<String>,
    metadata: JsonValue,
    provenance: JsonValue,
    max_bytes: usize,
}

#[derive(Debug)]
struct ValidatedArtifactSpec {
    spec: JsonValue,
    fallback: String,
    size_bytes: u64,
}

#[harn_builtin(
    sig = "artifact_emit(kind: string, spec: any, options?: dict) -> dict",
    kind = "async",
    category = "agent.artifact",
    doc = "Validate and emit a declarative renderable artifact event for the current agent session."
)]
async fn artifact_emit_builtin(
    ctx: AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let kind = match args.first() {
        Some(VmValue::String(text)) if !text.trim().is_empty() => ArtifactKind::parse(text)?,
        Some(other) => {
            return Err(err(format!(
                "`kind` must be a non-empty string; got {}",
                other.type_name()
            )));
        }
        None => return Err(err("missing `kind`")),
    };
    let raw_spec = args
        .get(1)
        .ok_or_else(|| err("missing `spec`"))
        .map(crate::llm::vm_value_to_json)?;
    let options = parse_options(args.get(2))?;
    let mut validated = validate_artifact_spec(kind, raw_spec, options.max_bytes)?;
    if let Some(fallback) = options.fallback {
        validate_fallback(&fallback)?;
        validated.fallback = fallback;
    }

    let event = AgentEvent::Artifact {
        session_id: options.session_id.clone(),
        artifact_id: options.artifact_id.clone(),
        kind: kind.as_str().to_string(),
        title: options.title.clone(),
        mime_type: kind.mime_type().to_string(),
        spec: validated.spec.clone(),
        fallback: validated.fallback.clone(),
        size_bytes: validated.size_bytes,
        provenance: options.provenance.clone(),
        metadata: options.metadata.clone(),
    };

    if crate::agent_sessions::exists(&options.session_id) {
        append_transcript_event(&event)?;
    }
    crate::llm::emit_live_agent_event_with_ctx(Some(&ctx), &event).await;

    Ok(crate::stdlib::json_to_vm_value(&json!({
        "ok": true,
        "artifact_id": options.artifact_id,
        "kind": kind.as_str(),
        "title": options.title,
        "mime_type": kind.mime_type(),
        "size_bytes": validated.size_bytes,
        "metadata": options.metadata,
        "provenance": options.provenance,
    })))
}

fn err(message: impl Into<String>) -> VmError {
    VmError::Runtime(format!("artifact_emit: {}", message.into()))
}

fn parse_options(value: Option<&VmValue>) -> Result<ArtifactEmitOptions, VmError> {
    let opts = match value {
        None | Some(VmValue::Nil) => BTreeMap::new(),
        Some(VmValue::Dict(map)) => map.as_ref().clone(),
        Some(other) => {
            return Err(err(format!(
                "`options` must be a dict or nil; got {}",
                other.type_name()
            )));
        }
    };
    const KEYS: &[&str] = &[
        "artifact_id",
        "fallback",
        "id",
        "max_bytes",
        "metadata",
        "provenance",
        "session_id",
        "title",
    ];
    for key in opts.keys() {
        if !KEYS.contains(&key.as_str()) {
            return Err(err(format!(
                "unknown option key '{key}' (expected one of: {})",
                KEYS.join(", ")
            )));
        }
    }
    let session_id = opt_string(&opts, "session_id")?
        .or_else(crate::llm::current_agent_session_id)
        .ok_or_else(|| err("no active agent session; pass options.session_id"))?;
    let artifact_id = opt_string(&opts, "artifact_id")?
        .or(opt_string(&opts, "id")?)
        .unwrap_or_else(|| format!("artifact_{}", uuid::Uuid::now_v7()));
    let max_bytes = opt_int(&opts, "max_bytes")?
        .map(|value| {
            if value <= 0 {
                return Err(err("`max_bytes` must be > 0"));
            }
            let value = value as usize;
            if value > HARD_MAX_BYTES {
                return Err(err(format!(
                    "`max_bytes` must be <= {HARD_MAX_BYTES} bytes"
                )));
            }
            Ok(value)
        })
        .transpose()?
        .unwrap_or(DEFAULT_MAX_BYTES);
    Ok(ArtifactEmitOptions {
        session_id,
        artifact_id,
        title: opt_string(&opts, "title")?,
        fallback: opt_string(&opts, "fallback")?,
        metadata: opt_object(&opts, "metadata")?.unwrap_or_else(|| json!({})),
        provenance: opt_object(&opts, "provenance")?.unwrap_or_else(|| json!({})),
        max_bytes,
    })
}

fn opt_string(opts: &BTreeMap<String, VmValue>, key: &str) -> Result<Option<String>, VmError> {
    match opts.get(key) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::String(value)) => {
            let value = value.trim();
            if value.len() > MAX_STRING_BYTES {
                return Err(err(format!("`{key}` exceeds {MAX_STRING_BYTES} bytes")));
            }
            if value.is_empty() {
                Ok(None)
            } else {
                Ok(Some(value.to_string()))
            }
        }
        Some(other) => Err(err(format!(
            "`{key}` must be a string or nil; got {}",
            other.type_name()
        ))),
    }
}

fn opt_int(opts: &BTreeMap<String, VmValue>, key: &str) -> Result<Option<i64>, VmError> {
    match opts.get(key) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(value) => value
            .as_int()
            .map(Some)
            .ok_or_else(|| err(format!("`{key}` must be an int"))),
    }
}

fn opt_object(opts: &BTreeMap<String, VmValue>, key: &str) -> Result<Option<JsonValue>, VmError> {
    match opts.get(key) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::Dict(_)) => {
            let value = crate::llm::vm_value_to_json(opts.get(key).expect("checked above"));
            if value.is_object() {
                let size = serde_json::to_vec(&value)
                    .map_err(|error| err(format!("failed to encode `{key}`: {error}")))?
                    .len();
                if size > MAX_METADATA_BYTES {
                    return Err(err(format!(
                        "`{key}` is {size} bytes; max is {MAX_METADATA_BYTES}"
                    )));
                }
                Ok(Some(value))
            } else {
                Err(err(format!("`{key}` must be a dict")))
            }
        }
        Some(other) => Err(err(format!(
            "`{key}` must be a dict or nil; got {}",
            other.type_name()
        ))),
    }
}

fn validate_artifact_spec(
    kind: ArtifactKind,
    spec: JsonValue,
    max_bytes: usize,
) -> Result<ValidatedArtifactSpec, VmError> {
    let spec = match kind {
        ArtifactKind::VegaLite => validate_vega_lite(spec)?,
        ArtifactKind::Mermaid => validate_mermaid(spec)?,
        ArtifactKind::Table => validate_table(spec)?,
    };
    let size_bytes = serialized_size(&spec, max_bytes)?;
    let fallback = default_fallback(kind, &spec)?;
    Ok(ValidatedArtifactSpec {
        spec,
        fallback,
        size_bytes,
    })
}

fn validate_vega_lite(spec: JsonValue) -> Result<JsonValue, VmError> {
    let object = spec
        .as_object()
        .ok_or_else(|| err("vega-lite spec must be a JSON object"))?;
    if let Some(schema) = object.get("$schema") {
        let schema = schema
            .as_str()
            .ok_or_else(|| err("vega-lite `$schema` must be a string"))?;
        if !schema.starts_with("https://vega.github.io/schema/vega-lite/") {
            return Err(err(
                "vega-lite `$schema` must reference the Vega-Lite schema",
            ));
        }
    }
    security_scan(&spec, "spec", ArtifactKind::VegaLite)?;
    if !has_vega_visual_root(object) {
        return Err(err(
            "vega-lite spec must include a mark/encoding chart or a composite chart",
        ));
    }
    validate_vega_data_refs(&spec, "spec")?;
    Ok(spec)
}

fn has_vega_visual_root(object: &serde_json::Map<String, JsonValue>) -> bool {
    object.contains_key("mark") && object.contains_key("encoding")
        || ["layer", "hconcat", "vconcat", "concat"].iter().any(|key| {
            object
                .get(*key)
                .and_then(JsonValue::as_array)
                .map(|items| !items.is_empty())
                .unwrap_or(false)
        })
        || object.contains_key("facet")
        || object.contains_key("repeat")
}

fn validate_vega_data_refs(value: &JsonValue, path: &str) -> Result<(), VmError> {
    match value {
        JsonValue::Object(object) => {
            for (key, child) in object {
                let child_path = format!("{path}.{key}");
                if key == "data" {
                    validate_vega_data(child, &child_path)?;
                }
                validate_vega_data_refs(child, &child_path)?;
            }
        }
        JsonValue::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                validate_vega_data_refs(child, &format!("{path}[{index}]"))?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_vega_data(value: &JsonValue, path: &str) -> Result<(), VmError> {
    let Some(object) = value.as_object() else {
        return Err(err(format!("{path} must be an object")));
    };
    if object.contains_key("url") {
        return Err(err(format!(
            "{path}.url is an external data reference; inline data.values or use a resource reference"
        )));
    }
    if let Some(values) = object.get("values") {
        match values {
            JsonValue::Array(_) | JsonValue::Object(_) => {}
            _ => return Err(err(format!("{path}.values must be an array or object"))),
        }
    }
    Ok(())
}

fn validate_mermaid(spec: JsonValue) -> Result<JsonValue, VmError> {
    security_scan(&spec, "spec", ArtifactKind::Mermaid)?;
    let code = match &spec {
        JsonValue::String(text) => text.trim().to_string(),
        JsonValue::Object(object) => object
            .get("code")
            .and_then(JsonValue::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .ok_or_else(|| err("mermaid spec object must include a non-empty `code` string"))?,
        _ => {
            return Err(err(
                "mermaid spec must be a string or {code: string} object",
            ))
        }
    };
    if code.len() > MAX_MERMAID_BYTES {
        return Err(err(format!(
            "mermaid spec exceeds {MAX_MERMAID_BYTES} bytes"
        )));
    }
    let first = first_mermaid_directive(&code)
        .ok_or_else(|| err("mermaid spec must include a diagram directive"))?;
    if !is_allowed_mermaid_directive(first) {
        return Err(err(format!(
            "unsupported mermaid diagram directive '{first}'"
        )));
    }
    Ok(json!({ "code": code }))
}

fn first_mermaid_directive(code: &str) -> Option<&str> {
    code.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("%%"))
        .find_map(|line| line.split_whitespace().next())
}

fn is_allowed_mermaid_directive(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "flowchart"
            | "graph"
            | "sequencediagram"
            | "classdiagram"
            | "statediagram"
            | "statediagram-v2"
            | "erdiagram"
            | "journey"
            | "gantt"
            | "pie"
            | "mindmap"
            | "timeline"
            | "quadrantchart"
            | "gitgraph"
            | "requirementdiagram"
            | "c4context"
            | "c4container"
            | "c4component"
            | "c4dynamic"
            | "block-beta"
            | "packet-beta"
            | "xychart-beta"
            | "sankey-beta"
    )
}

fn validate_table(spec: JsonValue) -> Result<JsonValue, VmError> {
    security_scan(&spec, "spec", ArtifactKind::Table)?;
    let object = spec
        .as_object()
        .ok_or_else(|| err("table spec must be a JSON object"))?;
    let columns = object
        .get("columns")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| err("table spec must include a `columns` array"))?;
    if columns.is_empty() {
        return Err(err("table columns must not be empty"));
    }
    if columns.len() > MAX_TABLE_COLUMNS {
        return Err(err(format!(
            "table has {} columns; max is {MAX_TABLE_COLUMNS}",
            columns.len()
        )));
    }
    let column_names = columns
        .iter()
        .enumerate()
        .map(|(index, column)| table_column_name(column, index))
        .collect::<Result<Vec<_>, _>>()?;
    let rows = object
        .get("rows")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| err("table spec must include a `rows` array"))?;
    if rows.len() > MAX_TABLE_ROWS {
        return Err(err(format!(
            "table has {} rows; max is {MAX_TABLE_ROWS}",
            rows.len()
        )));
    }
    for (index, row) in rows.iter().enumerate() {
        match row {
            JsonValue::Array(cells) => {
                if cells.len() > column_names.len() {
                    return Err(err(format!(
                        "table row {index} has {} cells but only {} columns",
                        cells.len(),
                        column_names.len()
                    )));
                }
            }
            JsonValue::Object(_) => {}
            _ => return Err(err(format!("table row {index} must be an array or object"))),
        }
    }
    Ok(spec)
}

fn table_column_name(column: &JsonValue, index: usize) -> Result<String, VmError> {
    match column {
        JsonValue::String(name) if !name.trim().is_empty() => Ok(name.trim().to_string()),
        JsonValue::Object(object) => ["name", "key", "id", "title"]
            .iter()
            .find_map(|key| {
                object
                    .get(*key)
                    .and_then(JsonValue::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
            })
            .ok_or_else(|| {
                err(format!(
                    "table column {index} must include name/key/id/title"
                ))
            }),
        _ => Err(err(format!(
            "table column {index} must be a string or object"
        ))),
    }
}

fn serialized_size(spec: &JsonValue, max_bytes: usize) -> Result<u64, VmError> {
    let bytes = serde_json::to_vec(spec)
        .map_err(|error| err(format!("failed to encode spec as JSON: {error}")))?;
    if bytes.len() > max_bytes {
        return Err(err(format!(
            "spec is {} bytes; max is {max_bytes}",
            bytes.len()
        )));
    }
    Ok(bytes.len() as u64)
}

fn validate_fallback(value: &str) -> Result<(), VmError> {
    if value.len() > MAX_FALLBACK_BYTES {
        return Err(err(format!(
            "`fallback` exceeds {MAX_FALLBACK_BYTES} bytes"
        )));
    }
    scan_string("options.fallback", value, false)
}

fn security_scan(value: &JsonValue, path: &str, kind: ArtifactKind) -> Result<(), VmError> {
    match value {
        JsonValue::Object(object) => {
            for (key, child) in object {
                let child_path = format!("{path}.{key}");
                if is_external_ref_key(key) && !child.is_null() {
                    return Err(err(format!(
                        "{child_path} is an external reference; inline data or use an artifact/resource reference"
                    )));
                }
                if key == "$schema" && kind != ArtifactKind::VegaLite {
                    return Err(err(format!(
                        "{child_path} is not allowed for {}",
                        kind.as_str()
                    )));
                }
                security_scan(child, &child_path, kind)?;
            }
        }
        JsonValue::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                security_scan(child, &format!("{path}[{index}]"), kind)?;
            }
        }
        JsonValue::String(text) => {
            let allow_schema = kind == ArtifactKind::VegaLite && path.ends_with(".$schema");
            scan_string(path, text, allow_schema)?;
        }
        _ => {}
    }
    Ok(())
}

fn is_external_ref_key(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "href" | "src" | "url" | "xlink:href"
    )
}

fn scan_string(path: &str, value: &str, allow_vega_schema_url: bool) -> Result<(), VmError> {
    if value.len() > MAX_STRING_BYTES {
        return Err(err(format!("{path} exceeds {MAX_STRING_BYTES} bytes")));
    }
    let lower = value.to_ascii_lowercase();
    for marker in [
        "<script",
        "</script",
        "<svg",
        "</svg",
        "<foreignobject",
        "javascript:",
        "data:text/html",
        "data:image/svg",
        "onload=",
        "onclick=",
        "onerror=",
    ] {
        if lower.contains(marker) {
            return Err(err(format!(
                "{path} contains unsafe payload marker `{marker}`"
            )));
        }
    }
    if contains_external_url(&lower) {
        if allow_vega_schema_url && value.starts_with("https://vega.github.io/schema/vega-lite/") {
            return Ok(());
        }
        return Err(err(format!(
            "{path} contains an external reference; renderers must not fetch network resources"
        )));
    }
    Ok(())
}

fn contains_external_url(value: &str) -> bool {
    value.contains("http://") || value.contains("https://") || value.contains("://")
}

fn default_fallback(kind: ArtifactKind, spec: &JsonValue) -> Result<String, VmError> {
    match kind {
        ArtifactKind::VegaLite => Ok(default_vega_fallback(spec)),
        ArtifactKind::Mermaid => Ok(spec
            .get("code")
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .to_string()),
        ArtifactKind::Table => default_table_fallback(spec),
    }
}

fn default_vega_fallback(spec: &JsonValue) -> String {
    let title = spec
        .get("title")
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let mark = spec
        .get("mark")
        .and_then(|mark| match mark {
            JsonValue::String(text) => Some(text.as_str()),
            JsonValue::Object(object) => object.get("type").and_then(JsonValue::as_str),
            _ => None,
        })
        .unwrap_or("composite");
    match title {
        Some(title) => format!("{title} ({mark} chart)"),
        None => format!("Vega-Lite {mark} chart"),
    }
}

fn default_table_fallback(spec: &JsonValue) -> Result<String, VmError> {
    let object = spec
        .as_object()
        .ok_or_else(|| err("table spec must be a JSON object"))?;
    let columns = object
        .get("columns")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| err("table spec must include a `columns` array"))?;
    let column_names = columns
        .iter()
        .enumerate()
        .map(|(index, column)| table_column_name(column, index))
        .collect::<Result<Vec<_>, _>>()?;
    let mut lines = Vec::new();
    lines.push(column_names.join(" | "));
    if let Some(rows) = object.get("rows").and_then(JsonValue::as_array) {
        for row in rows.iter().take(10) {
            lines.push(table_row_text(row, &column_names));
        }
        if rows.len() > 10 {
            lines.push(format!("... {} more rows", rows.len() - 10));
        }
    }
    Ok(lines.join("\n"))
}

fn table_row_text(row: &JsonValue, columns: &[String]) -> String {
    match row {
        JsonValue::Array(cells) => cells.iter().map(cell_text).collect::<Vec<_>>().join(" | "),
        JsonValue::Object(object) => columns
            .iter()
            .map(|column| object.get(column).map(cell_text).unwrap_or_default())
            .collect::<Vec<_>>()
            .join(" | "),
        _ => String::new(),
    }
}

fn cell_text(value: &JsonValue) -> String {
    match value {
        JsonValue::Null => String::new(),
        JsonValue::String(text) => text.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

fn append_transcript_event(event: &AgentEvent) -> Result<(), VmError> {
    let AgentEvent::Artifact {
        session_id,
        artifact_id,
        kind,
        title,
        mime_type,
        spec,
        fallback,
        size_bytes,
        provenance,
        metadata,
    } = event
    else {
        return Ok(());
    };
    let transcript_metadata = json!({
        "artifactId": artifact_id,
        "kind": kind,
        "title": title,
        "mimeType": mime_type,
        "spec": spec,
        "fallback": fallback,
        "sizeBytes": size_bytes,
        "provenance": provenance,
        "metadata": metadata,
    });
    let transcript_event = crate::llm::helpers::transcript_event(
        "artifact",
        "assistant",
        "public",
        fallback,
        Some(transcript_metadata),
    );
    crate::agent_sessions::append_event(session_id, transcript_event).map_err(err)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_error_contains(result: Result<ValidatedArtifactSpec, VmError>, expected: &str) {
        let error = result.expect_err("validation should fail");
        let text = error.to_string();
        assert!(
            text.contains(expected),
            "expected error containing {expected:?}, got {text:?}"
        );
    }

    #[test]
    fn validates_supported_artifact_kinds() {
        let vega = json!({
            "mark": "bar",
            "data": {"values": [{"name": "a", "count": 2}]},
            "encoding": {
                "x": {"field": "name", "type": "nominal"},
                "y": {"field": "count", "type": "quantitative"}
            }
        });
        let mermaid = JsonValue::String("flowchart TD\n  A --> B".to_string());
        let table = json!({
            "columns": ["name", "count"],
            "rows": [{"name": "a", "count": 2}]
        });

        let vega = validate_artifact_spec(ArtifactKind::VegaLite, vega, DEFAULT_MAX_BYTES)
            .expect("vega-lite validates");
        assert_eq!(vega.spec["mark"], "bar");
        let mermaid = validate_artifact_spec(ArtifactKind::Mermaid, mermaid, DEFAULT_MAX_BYTES)
            .expect("mermaid validates");
        assert_eq!(mermaid.spec["code"], "flowchart TD\n  A --> B");
        let table = validate_artifact_spec(ArtifactKind::Table, table, DEFAULT_MAX_BYTES)
            .expect("table validates");
        assert!(table.fallback.contains("name | count"));
    }

    #[test]
    fn rejects_unsafe_payloads_and_external_refs() {
        assert_error_contains(
            validate_artifact_spec(
                ArtifactKind::Mermaid,
                JsonValue::String("flowchart TD\nA[<script>alert(1)</script>]".to_string()),
                DEFAULT_MAX_BYTES,
            ),
            "unsafe payload marker",
        );
        assert_error_contains(
            validate_artifact_spec(
                ArtifactKind::Table,
                json!({"columns": ["svg"], "rows": [["<svg onload=alert(1)>"]]}),
                DEFAULT_MAX_BYTES,
            ),
            "unsafe payload marker",
        );
        assert_error_contains(
            validate_artifact_spec(
                ArtifactKind::VegaLite,
                json!({
                    "mark": "line",
                    "data": {"url": "https://example.com/data.csv"},
                    "encoding": {"x": {"field": "x"}, "y": {"field": "y"}}
                }),
                DEFAULT_MAX_BYTES,
            ),
            "external reference",
        );
    }

    #[test]
    fn rejects_oversized_or_malformed_specs() {
        assert_error_contains(
            validate_artifact_spec(
                ArtifactKind::Mermaid,
                JsonValue::String("notDiagram TD\nA-->B".to_string()),
                DEFAULT_MAX_BYTES,
            ),
            "unsupported mermaid diagram directive",
        );
        assert_error_contains(
            validate_artifact_spec(
                ArtifactKind::Table,
                json!({"columns": [], "rows": []}),
                DEFAULT_MAX_BYTES,
            ),
            "columns must not be empty",
        );
        assert_error_contains(
            validate_artifact_spec(
                ArtifactKind::Table,
                json!({"columns": ["a"], "rows": [{"a": "value"}]}),
                10,
            ),
            "max is 10",
        );
    }
}
