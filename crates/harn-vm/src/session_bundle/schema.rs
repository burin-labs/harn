use serde_json::{json, Value as JsonValue};

use super::{
    SessionBundleError, SESSION_BUNDLE_SCHEMA_ID, SESSION_BUNDLE_SCHEMA_VERSION,
    SESSION_BUNDLE_TYPE,
};

pub fn session_bundle_schema() -> JsonValue {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": SESSION_BUNDLE_SCHEMA_ID,
        "title": "Harn Session Bundle v1",
        "type": "object",
        "additionalProperties": true,
        "required": [
            "_type",
            "schema_version",
            "bundle_id",
            "created_at",
            "producer",
            "source",
            "runtime",
            "transcript",
            "tools",
            "permissions",
            "replay",
            "redaction",
            "attachments"
        ],
        "properties": {
            "_type": {"const": SESSION_BUNDLE_TYPE},
            "schema_version": {"type": "integer", "minimum": 1, "maximum": SESSION_BUNDLE_SCHEMA_VERSION},
            "bundle_id": {"type": "string", "minLength": 1},
            "created_at": {"type": "string", "minLength": 1},
            "producer": {
                "type": "object",
                "required": ["name", "version", "schema_id"],
                "additionalProperties": true,
                "properties": {
                    "name": {"type": "string"},
                    "version": {"type": "string"},
                    "schema_id": {"type": "string"}
                }
            },
            "source": {
                "type": "object",
                "required": ["kind", "run_record_id", "workflow_id", "task", "status"],
                "additionalProperties": true,
                "properties": {
                    "kind": {"type": "string"},
                    "run_record_id": {"type": "string"},
                    "workflow_id": {"type": "string"},
                    "workflow_name": {"type": ["string", "null"]},
                    "task": {"type": "string"},
                    "status": {"type": "string"},
                    "started_at": {"type": "string"},
                    "finished_at": {"type": ["string", "null"]},
                    "persisted_path": {"type": ["string", "null"]},
                    "root_run_id": {"type": ["string", "null"]},
                    "parent_run_id": {"type": ["string", "null"]},
                    "child_run_count": {"type": "integer", "minimum": 0}
                }
            },
            "runtime": {
                "type": "object",
                "required": ["harn_version", "provider_models"],
                "additionalProperties": true,
                "properties": {
                    "harn_version": {"type": "string"},
                    "provider_models": {"type": "array", "items": {"type": "string"}},
                    "usage": {"type": ["object", "null"]},
                    "metadata": {"type": "object"}
                }
            },
            "workspace": {"type": ["object", "null"]},
            "transcript": {
                "type": "object",
                "required": ["sections"],
                "additionalProperties": true,
                "properties": {
                    "sections": {
                        "type": "array",
                        "items": {"$ref": "#/$defs/transcript_section"}
                    }
                }
            },
            "tools": {
                "type": "object",
                "required": ["schemas", "calls"],
                "additionalProperties": true,
                "properties": {
                    "schemas": {"type": "array", "items": {"$ref": "#/$defs/json_entry"}},
                    "calls": {"type": "array", "items": {"type": "object"}}
                }
            },
            "permissions": {"type": "array", "items": {"type": "object"}},
            "replay": {
                "type": "object",
                "required": ["event_log_pointers", "transitions", "checkpoints", "trace_spans", "deterministic_events"],
                "additionalProperties": true,
                "properties": {
                    "replay_fixture": {"type": ["object", "null"]},
                    "run_record": {"type": ["object", "null"]},
                    "observability": {"type": ["object", "null"]},
                    "verification_outcomes": {"type": "array", "items": {"type": "object"}},
                    "event_log_pointers": {"type": "array", "items": {"type": "object"}},
                    "transitions": {"type": "array", "items": {"type": "object"}},
                    "checkpoints": {"type": "array", "items": {"type": "object"}},
                    "trace_spans": {"type": "array", "items": {"type": "object"}},
                    "deterministic_events": {"type": "array", "items": {"$ref": "#/$defs/json_entry"}}
                }
            },
            "redaction": {
                "type": "object",
                "required": ["mode", "policy", "placeholder", "entries", "unsafe_secret_markers_rejected"],
                "additionalProperties": true,
                "properties": {
                    "mode": {"enum": ["local", "sanitized", "replay_only"]},
                    "policy": {"type": "string"},
                    "placeholder": {"type": "string"},
                    "entries": {"type": "array", "items": {"type": "object"}},
                    "unsafe_secret_markers_rejected": {"type": "boolean"}
                }
            },
            "attachments": {"type": "array", "items": {"type": "object"}},
            "metadata": {"type": "object"}
        },
        "$defs": {
            "json_entry": {
                "type": "object",
                "required": ["source", "index", "value"],
                "additionalProperties": true,
                "properties": {
                    "source": {"type": "string"},
                    "index": {"type": "integer", "minimum": 0},
                    "value": true
                }
            },
            "transcript_section": {
                "type": "object",
                "required": ["id", "label", "scope", "location", "messages", "events", "assets", "metadata"],
                "additionalProperties": true,
                "properties": {
                    "id": {"type": "string"},
                    "label": {"type": "string"},
                    "scope": {"type": "string"},
                    "location": {"type": "string"},
                    "summary": {"type": ["string", "null"]},
                    "messages": {"type": "array", "items": true},
                    "events": {"type": "array", "items": true},
                    "assets": {"type": "array", "items": true},
                    "metadata": {"type": "object"}
                }
            }
        }
    })
}

pub fn session_bundle_schema_pretty() -> Result<String, SessionBundleError> {
    serde_json::to_string_pretty(&session_bundle_schema())
        .map(|schema| format!("{schema}\n"))
        .map_err(|error| SessionBundleError::Encode(error.to_string()))
}
