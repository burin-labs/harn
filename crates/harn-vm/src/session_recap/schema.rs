use serde_json::{json, Value};

/// Closed schema-v1 write contract for the deterministic recap projection.
///
/// Readers may tolerate additive unknown fields, but writers preserve
/// forward-compatible data only through the explicit `extensions` object.
/// This prevents decode/re-encode cycles from silently claiming unknown
/// top-level fields as part of schema v1.
pub fn session_recap_json_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": "https://harnlang.com/schemas/session-recap-v1.schema.json",
        "title": "Harn session recap availability",
        "oneOf": [
            {
                "type": "object",
                "additionalProperties": false,
                "required": ["state", "snapshot"],
                "properties": {
                    "state": {"const": "available"},
                    "snapshot": {"$ref": "#/$defs/snapshot"}
                }
            },
            {
                "type": "object",
                "additionalProperties": false,
                "required": ["state", "reason"],
                "properties": {
                    "state": {"const": "unavailable"},
                    "reason": {"enum": [
                        "journal_unavailable",
                        "session_missing",
                        "projection_failed",
                        "admission_terminal"
                    ]}
                }
            }
        ],
        "$defs": {
            "nullableString": {"type": ["string", "null"]},
            "nullableInteger": {"type": ["integer", "null"]},
            "completionState": {"enum": ["open", "complete", "incomplete", "unassigned"]},
            "query": {
                "type": "object",
                "additionalProperties": false,
                "required": ["sessionId", "runId", "turnId", "fromEventId", "limit"],
                "properties": {
                    "sessionId": {"type": "string", "minLength": 1},
                    "runId": {"$ref": "#/$defs/nullableString"},
                    "turnId": {"$ref": "#/$defs/nullableString"},
                    "fromEventId": {"$ref": "#/$defs/nullableInteger"},
                    "limit": {"$ref": "#/$defs/nullableInteger"}
                }
            },
            "cursor": {
                "type": "object",
                "additionalProperties": false,
                "required": ["lastEventId", "nextEventId"],
                "properties": {
                    "lastEventId": {"$ref": "#/$defs/nullableInteger"},
                    "nextEventId": {"$ref": "#/$defs/nullableInteger"}
                }
            },
            "coverage": {
                "type": "object",
                "additionalProperties": false,
                "required": ["scanned", "matched", "pending", "unassigned", "truncated"],
                "properties": {
                    "scanned": {"type": "integer", "minimum": 0},
                    "matched": {"type": "integer", "minimum": 0},
                    "pending": {"type": "integer", "minimum": 0},
                    "unassigned": {"type": "integer", "minimum": 0},
                    "truncated": {"type": "boolean"}
                }
            },
            "sourceEvent": {
                "type": "object",
                "additionalProperties": false,
                "required": ["eventId", "recordHash"],
                "properties": {
                    "eventId": {"type": "integer", "minimum": 0},
                    "recordHash": {"type": "string"}
                }
            },
            "source": {
                "type": "object",
                "additionalProperties": false,
                "required": ["firstEventId", "lastEventId", "events"],
                "properties": {
                    "firstEventId": {"$ref": "#/$defs/nullableInteger"},
                    "lastEventId": {"$ref": "#/$defs/nullableInteger"},
                    "events": {"type": "array", "items": {"$ref": "#/$defs/sourceEvent"}}
                }
            },
            "textFact": {
                "type": "object",
                "additionalProperties": false,
                "required": ["text", "sourceEventId"],
                "properties": {
                    "text": {"type": "string"},
                    "sourceEventId": {"type": "integer", "minimum": 0}
                }
            },
            "verification": {
                "type": "object",
                "additionalProperties": false,
                "required": ["schema", "status", "verifiedPaths", "sourceEventId"],
                "properties": {
                    "schema": {"type": "string"},
                    "status": {"type": "string"},
                    "verifiedPaths": {"type": "array", "items": {"type": "string"}},
                    "sourceEventId": {"type": "integer", "minimum": 0}
                }
            },
            "toolExchange": {
                "type": "object",
                "additionalProperties": false,
                "required": [
                    "toolCallId", "toolName", "state", "callObserved", "resultObserved",
                    "input", "output", "verification", "sourceEventIds"
                ],
                "properties": {
                    "toolCallId": {"type": "string"},
                    "toolName": {"$ref": "#/$defs/nullableString"},
                    "state": {"enum": ["open", "completed", "failed", "incomplete"]},
                    "callObserved": {"type": "boolean"},
                    "resultObserved": {"type": "boolean"},
                    "input": {},
                    "output": {},
                    "verification": {"oneOf": [{"$ref": "#/$defs/verification"}, {"type": "null"}]},
                    "sourceEventIds": {"type": "array", "items": {"type": "integer", "minimum": 0}}
                }
            },
            "planStep": {
                "type": "object",
                "additionalProperties": false,
                "required": ["id", "content", "status"],
                "properties": {
                    "id": {"type": "string"},
                    "content": {"type": "string"},
                    "status": {"enum": ["pending", "in_progress", "completed", "blocked", "cancelled"]}
                }
            },
            "planEvent": {
                "type": "object",
                "additionalProperties": false,
                "required": ["kind", "eventId", "inputRevisionId"],
                "properties": {
                    "kind": {"enum": ["created", "updated"]},
                    "eventId": {"type": "string"},
                    "inputRevisionId": {"$ref": "#/$defs/nullableString"}
                }
            },
            "planFact": {
                "type": "object",
                "additionalProperties": false,
                "required": [
                    "documentId", "revisionId", "title", "summary", "steps", "event", "sourceEventId"
                ],
                "properties": {
                    "documentId": {"type": "string"},
                    "revisionId": {"type": "string"},
                    "title": {"type": "string"},
                    "summary": {"type": "string"},
                    "steps": {"type": "array", "items": {"$ref": "#/$defs/planStep"}},
                    "event": {"oneOf": [{"$ref": "#/$defs/planEvent"}, {"type": "null"}]},
                    "sourceEventId": {"type": "integer", "minimum": 0}
                }
            },
            "progressEntry": {
                "type": "object",
                "additionalProperties": false,
                "required": ["content", "status", "priority"],
                "properties": {
                    "content": {"type": "string"},
                    "status": {"enum": ["pending", "in_progress", "completed"]},
                    "priority": {"oneOf": [
                        {"enum": ["high", "medium", "low"]},
                        {"type": "null"}
                    ]}
                }
            },
            "progressFact": {
                "type": "object",
                "additionalProperties": false,
                "required": ["message", "entries", "replace", "sourceEventId"],
                "properties": {
                    "message": {"$ref": "#/$defs/nullableString"},
                    "entries": {"type": "array", "items": {"$ref": "#/$defs/progressEntry"}},
                    "replace": {"type": "boolean"},
                    "sourceEventId": {"type": "integer", "minimum": 0}
                }
            },
            "terminalFact": {
                "type": "object",
                "additionalProperties": false,
                "required": [
                    "state", "finalStatus", "stopReason", "kind", "owner", "reason", "sourceEventId"
                ],
                "properties": {
                    "state": {"$ref": "#/$defs/completionState"},
                    "finalStatus": {"$ref": "#/$defs/nullableString"},
                    "stopReason": {"$ref": "#/$defs/nullableString"},
                    "kind": {"$ref": "#/$defs/nullableString"},
                    "owner": {"$ref": "#/$defs/nullableString"},
                    "reason": {"$ref": "#/$defs/nullableString"},
                    "sourceEventId": {"type": "integer", "minimum": 0}
                }
            },
            "iteration": {
                "type": "object",
                "additionalProperties": false,
                "required": [
                    "iteration", "state", "assistantText", "tools", "plans", "progress", "sourceEventIds"
                ],
                "properties": {
                    "iteration": {"$ref": "#/$defs/nullableInteger"},
                    "state": {"$ref": "#/$defs/completionState"},
                    "assistantText": {"type": "array", "items": {"$ref": "#/$defs/textFact"}},
                    "tools": {"type": "array", "items": {"$ref": "#/$defs/toolExchange"}},
                    "plans": {"type": "array", "items": {"$ref": "#/$defs/planFact"}},
                    "progress": {"type": "array", "items": {"$ref": "#/$defs/progressFact"}},
                    "sourceEventIds": {"type": "array", "items": {"type": "integer", "minimum": 0}}
                }
            },
            "turn": {
                "type": "object",
                "additionalProperties": false,
                "required": ["turnId", "runId", "state", "prompts", "iterations", "terminal", "sourceEventIds"],
                "properties": {
                    "turnId": {"type": "string"},
                    "runId": {"type": "string"},
                    "state": {"$ref": "#/$defs/completionState"},
                    "prompts": {"type": "array", "items": {"$ref": "#/$defs/textFact"}},
                    "iterations": {"type": "array", "items": {"$ref": "#/$defs/iteration"}},
                    "terminal": {"oneOf": [{"$ref": "#/$defs/terminalFact"}, {"type": "null"}]},
                    "sourceEventIds": {"type": "array", "items": {"type": "integer", "minimum": 0}}
                }
            },
            "snapshot": {
                "type": "object",
                "additionalProperties": false,
                "required": [
                    "schemaVersion", "sessionId", "query", "cursor", "coverage", "source",
                    "contentHash", "projectionHash", "turns", "extensions"
                ],
                "properties": {
                    "schemaVersion": {"const": 1},
                    "sessionId": {"type": "string", "minLength": 1},
                    "query": {"$ref": "#/$defs/query"},
                    "cursor": {"$ref": "#/$defs/cursor"},
                    "coverage": {"$ref": "#/$defs/coverage"},
                    "source": {"$ref": "#/$defs/source"},
                    "contentHash": {"type": "string", "pattern": "^sha256:"},
                    "projectionHash": {"type": "string", "pattern": "^sha256:"},
                    "turns": {"type": "array", "items": {"$ref": "#/$defs/turn"}},
                    "extensions": {"type": "object", "additionalProperties": true}
                }
            }
        }
    })
}
