//! Typed Harn `session/update` extension payloads.
//!
//! Canonical ACP variants already have host-language structs. Harn extensions
//! used to collapse to a discriminator plus `_meta` lump, so hosts re-picked
//! identity fields by hand and silently dropped new required ones. This table
//! is the single owner: dump every language from it, and fail if the live
//! extension registry or schema `$defs` grow a required identity field that
//! has no generated struct.

#[cfg(test)]
use harn_serve::adapters::acp::HARN_SESSION_UPDATE_EXTENSIONS;

#[derive(Clone, Copy)]
pub(super) enum PayloadFieldKind {
    NonEmptyString,
    String,
    U64,
    Bool,
    StringList,
    Json,
}

#[derive(Clone, Copy)]
pub(super) struct PayloadField {
    pub wire_name: &'static str,
    pub rust_name: &'static str,
    pub kind: PayloadFieldKind,
    pub required: bool,
    pub identity: bool,
}

#[derive(Clone, Copy)]
pub(super) struct SessionUpdatePayload {
    pub discriminator: &'static str,
    pub type_stem: &'static str,
    pub fields: &'static [PayloadField],
}

const fn field(
    wire_name: &'static str,
    rust_name: &'static str,
    kind: PayloadFieldKind,
    required: bool,
    identity: bool,
) -> PayloadField {
    PayloadField {
        wire_name,
        rust_name,
        kind,
        required,
        identity,
    }
}

const fn req_id(
    wire_name: &'static str,
    rust_name: &'static str,
    kind: PayloadFieldKind,
) -> PayloadField {
    field(wire_name, rust_name, kind, true, true)
}

const fn req(
    wire_name: &'static str,
    rust_name: &'static str,
    kind: PayloadFieldKind,
) -> PayloadField {
    field(wire_name, rust_name, kind, true, false)
}

const fn opt(
    wire_name: &'static str,
    rust_name: &'static str,
    kind: PayloadFieldKind,
) -> PayloadField {
    field(wire_name, rust_name, kind, false, false)
}

/// Every Harn `session/update` extension the adapter advertises, with the
/// identity fields hosts must decode as first-class properties.
pub(super) const TYPED_SESSION_UPDATE_PAYLOADS: &[SessionUpdatePayload] = &[
    SessionUpdatePayload {
        discriminator: "artifact",
        type_stem: "Artifact",
        fields: &[
            req_id(
                "artifactId",
                "artifact_id",
                PayloadFieldKind::NonEmptyString,
            ),
            opt("kind", "kind", PayloadFieldKind::String),
            opt("title", "title", PayloadFieldKind::String),
        ],
    },
    SessionUpdatePayload {
        discriminator: "available_commands_update",
        type_stem: "AvailableCommands",
        fields: &[req_id(
            "availableCommands",
            "available_commands",
            PayloadFieldKind::Json,
        )],
    },
    SessionUpdatePayload {
        discriminator: "fs_watch",
        type_stem: "FsWatch",
        fields: &[
            req_id(
                "subscriptionId",
                "subscription_id",
                PayloadFieldKind::NonEmptyString,
            ),
            req_id("events", "events", PayloadFieldKind::Json),
        ],
    },
    SessionUpdatePayload {
        discriminator: "handoff",
        type_stem: "Handoff",
        fields: &[
            req_id("handoffId", "handoff_id", PayloadFieldKind::NonEmptyString),
            req_id(
                "artifactId",
                "artifact_id",
                PayloadFieldKind::NonEmptyString,
            ),
            req_id("handoff", "handoff", PayloadFieldKind::Json),
        ],
    },
    SessionUpdatePayload {
        discriminator: "hitl_request",
        type_stem: "HitlRequest",
        fields: &[
            req_id("requestId", "request_id", PayloadFieldKind::NonEmptyString),
            req("kind", "kind", PayloadFieldKind::String),
            req("payload", "payload", PayloadFieldKind::Json),
        ],
    },
    SessionUpdatePayload {
        discriminator: "hitl_resolved",
        type_stem: "HitlResolved",
        fields: &[
            req_id("requestId", "request_id", PayloadFieldKind::NonEmptyString),
            req("kind", "kind", PayloadFieldKind::String),
            req("outcome", "outcome", PayloadFieldKind::Json),
        ],
    },
    SessionUpdatePayload {
        discriminator: "live_session_client",
        type_stem: "LiveSessionClient",
        fields: &[
            req_id("action", "action", PayloadFieldKind::NonEmptyString),
            opt("state", "state", PayloadFieldKind::Json),
        ],
    },
    SessionUpdatePayload {
        discriminator: "log",
        type_stem: "Log",
        fields: &[
            req_id("message", "message", PayloadFieldKind::NonEmptyString),
            opt("level", "level", PayloadFieldKind::String),
            opt("fields", "fields", PayloadFieldKind::Json),
        ],
    },
    SessionUpdatePayload {
        discriminator: "progress",
        type_stem: "Progress",
        fields: &[
            req_id("message", "message", PayloadFieldKind::NonEmptyString),
            opt("phase", "phase", PayloadFieldKind::String),
            opt("progress", "progress", PayloadFieldKind::U64),
            opt("total", "total", PayloadFieldKind::U64),
            opt("data", "data", PayloadFieldKind::Json),
        ],
    },
    SessionUpdatePayload {
        discriminator: "reminder_emitted",
        type_stem: "ReminderEmitted",
        fields: &[
            req_id(
                "reminderId",
                "reminder_id",
                PayloadFieldKind::NonEmptyString,
            ),
            opt("reminder", "reminder", PayloadFieldKind::Json),
        ],
    },
    SessionUpdatePayload {
        discriminator: "skill_activated",
        type_stem: "SkillActivated",
        fields: &[
            req_id("skillName", "skill_name", PayloadFieldKind::NonEmptyString),
            opt("iteration", "iteration", PayloadFieldKind::U64),
            opt("reason", "reason", PayloadFieldKind::String),
        ],
    },
    SessionUpdatePayload {
        discriminator: "skill_deactivated",
        type_stem: "SkillDeactivated",
        fields: &[
            req_id("skillName", "skill_name", PayloadFieldKind::NonEmptyString),
            opt("iteration", "iteration", PayloadFieldKind::U64),
        ],
    },
    SessionUpdatePayload {
        discriminator: "skill_narrow",
        type_stem: "SkillNarrow",
        fields: &[
            req_id(
                "removedTools",
                "removed_tools",
                PayloadFieldKind::StringList,
            ),
            req_id(
                "remainingTools",
                "remaining_tools",
                PayloadFieldKind::StringList,
            ),
            opt("reason", "reason", PayloadFieldKind::String),
        ],
    },
    SessionUpdatePayload {
        discriminator: "skill_scope_tools",
        type_stem: "SkillScopeTools",
        fields: &[
            req_id("skillName", "skill_name", PayloadFieldKind::NonEmptyString),
            req_id(
                "allowedTools",
                "allowed_tools",
                PayloadFieldKind::StringList,
            ),
        ],
    },
    SessionUpdatePayload {
        discriminator: "stance_transition",
        type_stem: "StanceTransition",
        fields: &[
            req_id("phase", "phase", PayloadFieldKind::NonEmptyString),
            opt("escapeTool", "escape_tool", PayloadFieldKind::String),
            opt(
                "allowedTools",
                "allowed_tools",
                PayloadFieldKind::StringList,
            ),
            opt("justification", "justification", PayloadFieldKind::String),
            opt("consent", "consent", PayloadFieldKind::String),
            opt("reason", "reason", PayloadFieldKind::String),
        ],
    },
    SessionUpdatePayload {
        discriminator: "tool_search_query",
        type_stem: "ToolSearchQuery",
        fields: &[
            req_id("toolUseId", "tool_use_id", PayloadFieldKind::NonEmptyString),
            req_id("name", "name", PayloadFieldKind::NonEmptyString),
            req("query", "query", PayloadFieldKind::Json),
            opt("strategy", "strategy", PayloadFieldKind::String),
            opt("mode", "mode", PayloadFieldKind::String),
        ],
    },
    SessionUpdatePayload {
        discriminator: "tool_search_result",
        type_stem: "ToolSearchResult",
        fields: &[
            req_id("toolUseId", "tool_use_id", PayloadFieldKind::NonEmptyString),
            req("promoted", "promoted", PayloadFieldKind::Json),
            opt("strategy", "strategy", PayloadFieldKind::String),
            opt("mode", "mode", PayloadFieldKind::String),
        ],
    },
    SessionUpdatePayload {
        discriminator: "transcript_compacted",
        type_stem: "TranscriptCompacted",
        fields: &[
            req_id("mode", "mode", PayloadFieldKind::String),
            req_id("strategy", "strategy", PayloadFieldKind::String),
        ],
    },
    SessionUpdatePayload {
        discriminator: "transcript_projected",
        type_stem: "TranscriptProjected",
        fields: &[
            req_id("policy", "policy", PayloadFieldKind::String),
            req("reason", "reason", PayloadFieldKind::String),
        ],
    },
    SessionUpdatePayload {
        discriminator: "worker_update",
        type_stem: "Worker",
        fields: &[
            req_id("workerId", "worker_id", PayloadFieldKind::NonEmptyString),
            req_id("event", "event", PayloadFieldKind::NonEmptyString),
            req_id("status", "status", PayloadFieldKind::NonEmptyString),
            req("terminal", "terminal", PayloadFieldKind::Bool),
            opt("workerName", "worker_name", PayloadFieldKind::String),
            opt("workerTask", "worker_task", PayloadFieldKind::String),
            opt("workerMode", "worker_mode", PayloadFieldKind::String),
            opt("metadata", "metadata", PayloadFieldKind::Json),
            opt("audit", "audit", PayloadFieldKind::Json),
        ],
    },
];

pub(super) fn ts_payload_type_name(payload: &SessionUpdatePayload) -> String {
    format!("ACP{}Update", payload.type_stem)
}

pub(super) fn rust_payload_type_name(payload: &SessionUpdatePayload) -> String {
    format!("ACP{}Update", payload.type_stem)
}

pub(super) fn swift_payload_type_name(payload: &SessionUpdatePayload) -> String {
    format!("HarnACP{}Update", payload.type_stem)
}

pub(super) fn python_payload_type_name(payload: &SessionUpdatePayload) -> String {
    format!("ACP{}Update", payload.type_stem)
}

pub(super) fn go_payload_type_name(payload: &SessionUpdatePayload) -> String {
    format!("ACP{}Update", payload.type_stem)
}

/// Fail the dump tests when the live adapter registry and this table diverge.
#[cfg(test)]
pub(super) fn typed_payload_registry_gaps() -> Vec<String> {
    let table: Vec<&str> = TYPED_SESSION_UPDATE_PAYLOADS
        .iter()
        .map(|payload| payload.discriminator)
        .collect();
    let mut gaps = Vec::new();
    for advertised in HARN_SESSION_UPDATE_EXTENSIONS {
        if !table.contains(advertised) {
            gaps.push(format!(
                "HARN_SESSION_UPDATE_EXTENSIONS lists `{advertised}` without a typed payload struct"
            ));
        }
    }
    for payload in TYPED_SESSION_UPDATE_PAYLOADS {
        if !HARN_SESSION_UPDATE_EXTENSIONS.contains(&payload.discriminator) {
            gaps.push(format!(
                "typed payload `{}` is not in HARN_SESSION_UPDATE_EXTENSIONS",
                payload.discriminator
            ));
        }
        if !payload
            .fields
            .iter()
            .any(|field| field.identity && field.required)
        {
            gaps.push(format!(
                "typed payload `{}` has no required identity field",
                payload.discriminator
            ));
        }
    }
    gaps
}

#[cfg(test)]
pub(super) fn schema_required_identity_gaps(schema_text: &str) -> Result<Vec<String>, String> {
    let schema: serde_json::Value = serde_json::from_str(schema_text)
        .map_err(|error| format!("session-update schema is not JSON: {error}"))?;
    let defs = schema
        .get("$defs")
        .and_then(|value| value.as_object())
        .ok_or_else(|| "session-update schema is missing $defs".to_string())?;
    let session_update = defs
        .get("SessionUpdate")
        .and_then(|value| value.get("oneOf"))
        .and_then(|value| value.as_array())
        .ok_or_else(|| "SessionUpdate.oneOf is missing".to_string())?;

    let mut gaps = Vec::new();
    for entry in session_update {
        let Some(def_name) = entry
            .get("$ref")
            .and_then(|value| value.as_str())
            .and_then(|reference| reference.strip_prefix("#/$defs/"))
        else {
            continue;
        };
        if matches!(
            def_name,
            "UserMessage"
                | "UserMessageChunk"
                | "AgentMessageChunk"
                | "AgentThoughtChunk"
                | "ToolCall"
                | "ToolCallUpdate"
                | "Plan"
                | "CurrentModeUpdate"
                | "ConfigOptionUpdate"
                | "SessionInfoUpdate"
                | "SessionTruncated"
                | "HarnExtensionUpdate"
        ) {
            continue;
        }
        let Some(payload) = TYPED_SESSION_UPDATE_PAYLOADS.iter().find(|payload| {
            payload.type_stem == def_name || format!("{}Update", payload.type_stem) == def_name
        }) else {
            gaps.push(format!(
                "schema $defs.{def_name} is in SessionUpdate.oneOf but has no typed dump struct"
            ));
            continue;
        };
        let Some(required) = defs
            .get(def_name)
            .and_then(|value| value.get("required"))
            .and_then(|value| value.as_array())
        else {
            continue;
        };
        for field_name in required.iter().filter_map(|value| value.as_str()) {
            if field_name == "sessionUpdate" || field_name == "_meta" {
                continue;
            }
            if !payload
                .fields
                .iter()
                .any(|field| field.wire_name == field_name && field.required)
            {
                gaps.push(format!(
                    "schema $defs.{def_name} requires `{field_name}` but the typed dump struct does not"
                ));
            }
        }
    }
    Ok(gaps)
}

pub(super) fn append_typescript_session_update_payloads(out: &mut String) {
    out.push_str(
        "\n/** Harn-owned `session/update` extension payloads. Identity fields are first-class; `_meta` remains for vendor extras. */\n",
    );
    out.push_str("export const HARN_TYPED_SESSION_UPDATE_PAYLOADS = {\n");
    for payload in TYPED_SESSION_UPDATE_PAYLOADS {
        let identity = payload
            .fields
            .iter()
            .filter(|field| field.identity)
            .map(|field| format!("\"{}\"", field.wire_name))
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&format!(
            "  {}: {{ typeName: \"{}\", identity: [{identity}] }},\n",
            payload.discriminator,
            ts_payload_type_name(payload)
        ));
    }
    out.push_str("} as const\n");
    for payload in TYPED_SESSION_UPDATE_PAYLOADS {
        out.push_str(&format!(
            "\nexport interface {} {{\n  sessionUpdate: \"{}\"\n",
            ts_payload_type_name(payload),
            payload.discriminator
        ));
        for field in payload.fields {
            let ts_type = match field.kind {
                PayloadFieldKind::NonEmptyString | PayloadFieldKind::String => "string",
                PayloadFieldKind::U64 => "number",
                PayloadFieldKind::Bool => "boolean",
                PayloadFieldKind::StringList => "string[]",
                PayloadFieldKind::Json => "ACPValue",
            };
            if field.required {
                out.push_str(&format!("  {}: {}\n", field.wire_name, ts_type));
            } else {
                out.push_str(&format!("  {}?: {}\n", field.wire_name, ts_type));
            }
        }
        out.push_str("  _meta?: ACPExtensionMeta<ACPObject>\n}\n");
    }
}

pub(super) fn typescript_session_update_union_members() -> String {
    let mut members = String::new();
    for payload in TYPED_SESSION_UPDATE_PAYLOADS {
        members.push_str(&format!("  | {}\n", ts_payload_type_name(payload)));
    }
    members
}

pub(super) fn append_rust_session_update_payloads(out: &mut String) {
    out.push_str(
        "\n/// Harn-owned `session/update` extension payloads. Identity fields are first-class.\n",
    );
    out.push_str("pub const HARN_TYPED_SESSION_UPDATE_PAYLOADS: &[(&str, &str, &[&str])] = &[\n");
    for payload in TYPED_SESSION_UPDATE_PAYLOADS {
        let identity = payload
            .fields
            .iter()
            .filter(|field| field.identity)
            .map(|field| format!("\"{}\"", field.wire_name))
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&format!(
            "    (\"{}\", \"{}\", &[{identity}]),\n",
            payload.discriminator,
            rust_payload_type_name(payload)
        ));
    }
    out.push_str("];\n");
    for payload in TYPED_SESSION_UPDATE_PAYLOADS {
        out.push_str(&format!(
            "\n#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]\n\
             #[serde(rename_all = \"camelCase\")]\n\
             pub struct {} {{\n\
             \x20   pub session_update: String,\n",
            rust_payload_type_name(payload)
        ));
        for field in payload.fields {
            let rust_type = rust_field_type(field);
            if field.required {
                out.push_str(&format!("    pub {}: {rust_type},\n", field.rust_name));
            } else {
                out.push_str("    #[serde(default, skip_serializing_if = \"Option::is_none\")]\n");
                out.push_str(&format!(
                    "    pub {}: Option<{rust_type}>,\n",
                    field.rust_name
                ));
            }
        }
        out.push_str(
            "    #[serde(default, skip_serializing_if = \"Option::is_none\", rename = \"_meta\")]\n\
             \x20   pub meta: Option<Value>,\n\
             }\n",
        );
    }
}

fn rust_field_type(field: &PayloadField) -> &'static str {
    match field.kind {
        PayloadFieldKind::NonEmptyString | PayloadFieldKind::String => "String",
        PayloadFieldKind::U64 => "u64",
        PayloadFieldKind::Bool => "bool",
        PayloadFieldKind::StringList => "Vec<String>",
        PayloadFieldKind::Json => "Value",
    }
}

pub(super) fn append_swift_session_update_payloads(out: &mut String) {
    out.push_str(
        "\n/// Harn-owned `session/update` extension payloads. Identity fields are first-class.\n",
    );
    for payload in TYPED_SESSION_UPDATE_PAYLOADS {
        out.push_str(&format!(
            "\npublic struct {}: Codable, Sendable, Equatable {{\n\
             \x20   public var sessionUpdate: HarnACPSessionUpdate\n",
            swift_payload_type_name(payload)
        ));
        for field in payload.fields {
            let swift_type = match field.kind {
                PayloadFieldKind::NonEmptyString | PayloadFieldKind::String => "String",
                PayloadFieldKind::U64 => "Int",
                PayloadFieldKind::Bool => "Bool",
                PayloadFieldKind::StringList => "[String]",
                PayloadFieldKind::Json => "HarnACPValue",
            };
            if field.required {
                out.push_str(&format!(
                    "    public var {}: {swift_type}\n",
                    field.wire_name
                ));
            } else {
                out.push_str(&format!(
                    "    public var {}: {swift_type}?\n",
                    field.wire_name
                ));
            }
        }
        out.push_str("    public var meta: HarnACPExtensionMeta?\n\n");
        out.push_str("    enum CodingKeys: String, CodingKey {\n        case sessionUpdate\n");
        for field in payload.fields {
            out.push_str(&format!("        case {}\n", field.wire_name));
        }
        out.push_str("        case meta = \"_meta\"\n    }\n}\n");
    }
}

pub(super) fn append_python_session_update_payloads(out: &mut String) {
    for payload in TYPED_SESSION_UPDATE_PAYLOADS {
        out.push_str(&format!(
            "\n@dataclass\nclass {}(_HarnDataclass):\n    sessionUpdate: str\n",
            python_payload_type_name(payload)
        ));
        let (required, optional): (Vec<&PayloadField>, Vec<&PayloadField>) =
            payload.fields.iter().partition(|field| field.required);
        for field in required {
            out.push_str(&format!(
                "    {}: {}\n",
                field.wire_name,
                python_field_type(field.kind, true)
            ));
        }
        for field in optional {
            out.push_str(&format!(
                "    {}: {} = None\n",
                field.wire_name,
                python_field_type(field.kind, false)
            ));
        }
        out.push_str("    _meta: Optional[HarnExtensionMeta] = None\n");
    }
}

fn python_field_type(kind: PayloadFieldKind, required: bool) -> String {
    let inner = match kind {
        PayloadFieldKind::NonEmptyString | PayloadFieldKind::String => "str",
        PayloadFieldKind::U64 => "int",
        PayloadFieldKind::Bool => "bool",
        PayloadFieldKind::StringList => "List[str]",
        PayloadFieldKind::Json => "JsonValue",
    };
    if required {
        inner.to_string()
    } else {
        format!("Optional[{inner}]")
    }
}

pub(super) fn append_go_session_update_payloads(out: &mut String) {
    for payload in TYPED_SESSION_UPDATE_PAYLOADS {
        out.push_str(&format!(
            "\n// {} is the typed Harn `{}` session/update payload.\ntype {} struct {{\n\tSessionUpdate string `json:\"sessionUpdate\"`\n",
            go_payload_type_name(payload),
            payload.discriminator,
            go_payload_type_name(payload)
        ));
        for field in payload.fields {
            let go_name = pascal_ident(field.wire_name);
            let (go_type, tag) = match (field.kind, field.required) {
                (PayloadFieldKind::NonEmptyString | PayloadFieldKind::String, true) => {
                    ("string", format!("json:\"{}\"", field.wire_name))
                }
                (PayloadFieldKind::NonEmptyString | PayloadFieldKind::String, false) => {
                    ("*string", format!("json:\"{},omitempty\"", field.wire_name))
                }
                (PayloadFieldKind::U64, true) => ("int", format!("json:\"{}\"", field.wire_name)),
                (PayloadFieldKind::U64, false) => {
                    ("*int", format!("json:\"{},omitempty\"", field.wire_name))
                }
                (PayloadFieldKind::Bool, true) => ("bool", format!("json:\"{}\"", field.wire_name)),
                (PayloadFieldKind::Bool, false) => {
                    ("*bool", format!("json:\"{},omitempty\"", field.wire_name))
                }
                (PayloadFieldKind::StringList, true) => {
                    ("[]string", format!("json:\"{}\"", field.wire_name))
                }
                (PayloadFieldKind::StringList, false) => (
                    "[]string",
                    format!("json:\"{},omitempty\"", field.wire_name),
                ),
                (PayloadFieldKind::Json, true) => {
                    ("json.RawMessage", format!("json:\"{}\"", field.wire_name))
                }
                (PayloadFieldKind::Json, false) => (
                    "json.RawMessage",
                    format!("json:\"{},omitempty\"", field.wire_name),
                ),
            };
            out.push_str(&format!("\t{go_name} {go_type} `{tag}`\n"));
        }
        out.push_str("\tMeta *HarnExtensionMeta `json:\"_meta,omitempty\"`\n}\n");
    }
}

fn pascal_ident(value: &str) -> String {
    let mut out = String::new();
    let mut capitalize = true;
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            if capitalize {
                out.extend(ch.to_uppercase());
            } else {
                out.push(ch);
            }
            capitalize = false;
        } else {
            capitalize = true;
        }
    }
    out
}
