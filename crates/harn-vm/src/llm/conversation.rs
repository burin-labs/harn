use crate::value::VmDictExt;

use crate::stdlib::macros::{harn_builtin, register_builtin_defs, VmBuiltinDef};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;
use harn_parser::diagnostic_codes::Code;

use super::helpers::{
    emit_reminder_lifecycle_event, is_transcript_value, new_transcript_with,
    new_transcript_with_events, normalize_transcript_asset, reminder_from_event,
    reminder_lifecycle_payload, transcript_asset_list, transcript_drain_decision_event_from_value,
    transcript_event, transcript_id, transcript_message_list, transcript_reminder_event,
    transcript_reminder_event_from_value, transcript_resumption_event_from_value,
    transcript_summary_text, transcript_suspension_event_from_value, vm_add_role_message,
    vm_message_value, vm_value_to_json, DirectiveAuthority, ReminderPropagate, ReminderRoleHint,
    ReminderSource, SystemReminder, REMINDER_DEDUPED_EVENT_KIND, REMINDER_EXPIRED_EVENT_KIND,
    REMINDER_INJECTED_EVENT_KIND, SYSTEM_REMINDER_EVENT_KIND,
};

pub(crate) const INJECT_REMINDER_KEYS: &[&str] = &[
    "body",
    "tags",
    "dedupe_key",
    "ttl_turns",
    "preserve_on_compact",
    "propagate",
    "role_hint",
    "authority",
];
const CLEAR_REMINDER_KEYS: &[&str] = &["id", "tag", "dedupe_key"];

/// Extract and validate a transcript dict from the first argument.
fn require_transcript<'a>(
    args: &'a [VmValue],
    context: &str,
) -> Result<&'a crate::value::DictMap, VmError> {
    match args.first() {
        Some(VmValue::Dict(d))
            if d.get("_type").map(|v| v.display()).as_deref() == Some("transcript") =>
        {
            Ok(d)
        }
        _ => Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            format!("{context}: argument must be a transcript"),
        )))),
    }
}

const CONVERSATION_BUILTINS: &[&VmBuiltinDef] = &[
    &TRANSCRIPT_IMPL_DEF,
    &TRANSCRIPT_FROM_MESSAGES_IMPL_DEF,
    &TRANSCRIPT_MESSAGES_IMPL_DEF,
    &TRANSCRIPT_ASSETS_IMPL_DEF,
    &TRANSCRIPT_EVENTS_IMPL_DEF,
    &TRANSCRIPT_REMINDER_EVENT_IMPL_DEF,
    &TRANSCRIPT_SUSPENSION_EVENT_IMPL_DEF,
    &TRANSCRIPT_RESUMPTION_EVENT_IMPL_DEF,
    &TRANSCRIPT_DRAIN_DECISION_EVENT_IMPL_DEF,
    &TRANSCRIPT_SUMMARY_IMPL_DEF,
    &TRANSCRIPT_ID_IMPL_DEF,
    &ADD_MESSAGE_IMPL_DEF,
    &ADD_USER_IMPL_DEF,
    &ADD_ASSISTANT_IMPL_DEF,
    &ADD_SYSTEM_IMPL_DEF,
    &ADD_TOOL_RESULT_IMPL_DEF,
    &TRANSCRIPT_FORK_IMPL_DEF,
    &TRANSCRIPT_RESET_IMPL_DEF,
    &TRANSCRIPT_ARCHIVE_IMPL_DEF,
    &TRANSCRIPT_ABANDON_IMPL_DEF,
    &TRANSCRIPT_RESUME_IMPL_DEF,
];

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "transcript(metadata?: dict) -> dict",
    category = "transcript"
)]
fn transcript_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(new_transcript_with(
        None,
        Vec::new(),
        None,
        args.first().cloned(),
    ))
}

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "transcript_from_messages(messages_or_transcript: list | dict) -> dict",
    category = "transcript"
)]
fn transcript_from_messages_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let messages = match args.first() {
        Some(VmValue::List(list)) => (**list).clone(),
        Some(VmValue::Dict(dict)) if is_transcript_value(&VmValue::Dict(dict.clone())) => {
            transcript_message_list(dict)?
        }
        _ => {
            return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                "transcript_from_messages: argument must be a message list or transcript",
            ))));
        }
    };
    Ok(new_transcript_with(None, messages, None, None))
}

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "transcript_messages(transcript: list | dict | Transcript) -> list",
    category = "transcript"
)]
fn transcript_messages_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let transcript = require_transcript(args, "transcript_messages")?;
    Ok(VmValue::List(std::sync::Arc::new(transcript_message_list(
        transcript,
    )?)))
}

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "transcript_assets(transcript: list | dict | Transcript) -> list",
    category = "transcript"
)]
fn transcript_assets_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let transcript = require_transcript(args, "transcript_assets")?;
    Ok(VmValue::List(std::sync::Arc::new(transcript_asset_list(
        transcript,
    )?)))
}

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "transcript_events(transcript: list | dict | Transcript) -> list",
    category = "transcript"
)]
fn transcript_events_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let transcript = require_transcript(args, "transcript_events")?;
    Ok(transcript
        .get("events")
        .cloned()
        .unwrap_or_else(|| VmValue::List(std::sync::Arc::new(Vec::new()))))
}

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "transcript_reminder_event(reminder: dict) -> dict",
    category = "transcript"
)]
fn transcript_reminder_event_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(transcript_reminder_event_from_value(
        args.first().unwrap_or(&VmValue::Nil),
    ))
}

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "transcript_suspension_event(suspension: dict) -> dict",
    category = "transcript"
)]
fn transcript_suspension_event_impl(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    Ok(transcript_suspension_event_from_value(
        args.first().unwrap_or(&VmValue::Nil),
    ))
}

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "transcript_resumption_event(resumption: dict) -> dict",
    category = "transcript"
)]
fn transcript_resumption_event_impl(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    Ok(transcript_resumption_event_from_value(
        args.first().unwrap_or(&VmValue::Nil),
    ))
}

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "transcript_drain_decision_event(drain: dict) -> dict",
    category = "transcript"
)]
fn transcript_drain_decision_event_impl(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    Ok(transcript_drain_decision_event_from_value(
        args.first().unwrap_or(&VmValue::Nil),
    ))
}

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "transcript_summary(transcript: list | dict | Transcript) -> string | nil",
    category = "transcript"
)]
fn transcript_summary_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let transcript = require_transcript(args, "transcript_summary")?;
    Ok(transcript.get("summary").cloned().unwrap_or(VmValue::Nil))
}

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "transcript_id(transcript: list | dict | Transcript) -> string",
    category = "transcript"
)]
fn transcript_id_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let transcript = require_transcript(args, "transcript_id")?;
    Ok(VmValue::String(arcstr::ArcStr::from(
        transcript_id(transcript).unwrap_or_default(),
    )))
}

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "add_message(messages_or_transcript: list | dict | Transcript, role: string, content: any) -> list | dict | Transcript",
    category = "transcript"
)]
fn add_message_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    match args.first() {
        Some(VmValue::List(list)) => {
            let role = args.get(1).map(VmValue::display).unwrap_or_default();
            let mut new_messages = (**list).clone();
            new_messages.push(vm_message_value(
                &role,
                args.get(2)
                    .cloned()
                    .unwrap_or_else(|| VmValue::String(arcstr::ArcStr::from(""))),
            ));
            Ok(VmValue::List(std::sync::Arc::new(new_messages)))
        }
        Some(VmValue::Dict(dict)) if is_transcript_value(&VmValue::Dict(dict.clone())) => {
            let role = args.get(1).map(VmValue::display).unwrap_or_default();
            let mut new_messages = transcript_message_list(dict)?;
            new_messages.push(vm_message_value(
                &role,
                args.get(2)
                    .cloned()
                    .unwrap_or_else(|| VmValue::String(arcstr::ArcStr::from(""))),
            ));
            Ok(rebuild_transcript(
                dict,
                new_messages,
                transcript_summary_text(dict),
                transcript_asset_list(dict)?,
                Vec::new(),
                transcript_state(dict),
            ))
        }
        _ => Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "add_message: first argument must be a message list or transcript",
        )))),
    }
}

#[harn_builtin(exposure = "pure", effects = [], sig = "add_user(messages_or_transcript: list | dict | Transcript, content: any) -> list | dict | Transcript", category = "transcript")]
fn add_user_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    vm_add_role_message(args, "user")
}

#[harn_builtin(exposure = "pure", effects = [], sig = "add_assistant(messages_or_transcript: list | dict | Transcript, content: any) -> list | dict | Transcript", category = "transcript")]
fn add_assistant_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    vm_add_role_message(args, "assistant")
}

#[harn_builtin(exposure = "pure", effects = [], sig = "add_system(messages_or_transcript: list | dict | Transcript, content: any) -> list | dict | Transcript", category = "transcript")]
fn add_system_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    vm_add_role_message(args, "system")
}

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "add_tool_result(messages_or_transcript: list | dict | Transcript, tool_use_id: string, content: any) -> list | dict | Transcript",
    category = "transcript"
)]
fn add_tool_result_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let build_message = || {
        let mut message = crate::value::DictMap::new();
        message.put_str("role", "tool_result");
        message.put_str(
            "tool_use_id",
            args.get(1).map(VmValue::display).unwrap_or_default(),
        );
        message.put_str(
            "content",
            args.get(2).map(VmValue::display).unwrap_or_default(),
        );
        VmValue::dict(message)
    };
    match args.first() {
        Some(VmValue::List(list)) => {
            let mut messages = (**list).clone();
            messages.push(build_message());
            Ok(VmValue::List(std::sync::Arc::new(messages)))
        }
        Some(VmValue::Dict(dict)) if is_transcript_value(&VmValue::Dict(dict.clone())) => {
            let mut messages = transcript_message_list(dict)?;
            messages.push(build_message());
            Ok(rebuild_transcript(
                dict,
                messages,
                transcript_summary_text(dict),
                transcript_asset_list(dict)?,
                Vec::new(),
                transcript_state(dict),
            ))
        }
        _ => Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "add_tool_result: first argument must be a message list or transcript",
        )))),
    }
}

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "transcript_fork(transcript: list | dict | Transcript, options?: dict) -> dict",
    category = "transcript"
)]
fn transcript_fork_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let transcript = require_transcript(args, "transcript_fork")?;
    let options = args.get(1).and_then(VmValue::as_dict);
    let retain_messages = options
        .and_then(|dict| dict.get("retain_messages"))
        .map(VmValue::is_truthy)
        .unwrap_or(true);
    let retain_summary = options
        .and_then(|dict| dict.get("retain_summary"))
        .map(VmValue::is_truthy)
        .unwrap_or(true);
    Ok(rebuild_transcript(
        transcript,
        if retain_messages {
            transcript_message_list(transcript)?
        } else {
            Vec::new()
        },
        if retain_summary {
            transcript_summary_text(transcript)
        } else {
            None
        },
        transcript_asset_list(transcript)?,
        vec![transcript_event(
            "transcript_fork",
            "system",
            "internal",
            "transcript forked",
            None,
        )],
        Some("forked"),
    ))
}

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "transcript_reset(options?: dict) -> dict",
    category = "transcript"
)]
fn transcript_reset_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let metadata = args
        .first()
        .and_then(VmValue::as_dict)
        .and_then(|dict| dict.get("metadata"))
        .cloned();
    Ok(new_transcript_with_events(
        None,
        Vec::new(),
        None,
        metadata,
        vec![transcript_event(
            "transcript_reset",
            "system",
            "internal",
            "transcript reset",
            None,
        )],
        Vec::new(),
        Some("active"),
    ))
}

fn transcript_with_state(
    args: &[VmValue],
    builtin: &str,
    event_text: &str,
    state: &str,
) -> Result<VmValue, VmError> {
    let transcript = require_transcript(args, builtin)?;
    Ok(rebuild_transcript(
        transcript,
        transcript_message_list(transcript)?,
        transcript_summary_text(transcript),
        transcript_asset_list(transcript)?,
        vec![transcript_event(
            builtin, "system", "internal", event_text, None,
        )],
        Some(state),
    ))
}

#[harn_builtin(exposure = "pure", effects = [], sig = "transcript_archive(transcript: list | dict | Transcript) -> dict", category = "transcript")]
fn transcript_archive_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    transcript_with_state(
        args,
        "transcript_archive",
        "transcript archived",
        "archived",
    )
}

#[harn_builtin(exposure = "pure", effects = [], sig = "transcript_abandon(transcript: list | dict | Transcript) -> dict", category = "transcript")]
fn transcript_abandon_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    transcript_with_state(
        args,
        "transcript_abandon",
        "transcript abandoned",
        "abandoned",
    )
}

#[harn_builtin(exposure = "pure", effects = [], sig = "transcript_resume(transcript: list | dict | Transcript) -> dict", category = "transcript")]
fn transcript_resume_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    transcript_with_state(args, "transcript_resume", "transcript resumed", "active")
}

#[harn_builtin(exposure = "pure", effects = [], sig = "conversation() -> list", category = "transcript")]
fn conversation_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(VmValue::List(std::sync::Arc::new(Vec::new())))
}

#[harn_builtin(exposure = "pure", effects = [], sig = "transcript_add_asset(transcript: list | dict | Transcript, asset: dict) -> Transcript", category = "transcript")]
fn transcript_add_asset_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let transcript = require_transcript(args, "transcript_add_asset")?;
    let asset_value = args.get(1).cloned().ok_or_else(|| {
        VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "transcript_add_asset: missing asset",
        )))
    })?;
    let normalized = normalize_transcript_asset(&asset_value);
    let asset_id = normalized
        .as_dict()
        .and_then(|dict| dict.get("id"))
        .map(VmValue::display)
        .unwrap_or_default();
    let mut assets = transcript_asset_list(transcript)?;
    assets.retain(|asset| {
        asset
            .as_dict()
            .and_then(|dict| dict.get("id"))
            .map(VmValue::display)
            .unwrap_or_default()
            != asset_id
    });
    assets.push(normalized);
    Ok(rebuild_transcript(
        transcript,
        transcript_message_list(transcript)?,
        transcript_summary_text(transcript),
        assets,
        Vec::new(),
        transcript_state(transcript),
    ))
}

#[harn_builtin(exposure = "pure", effects = [], sig = "transcript_render_visible(transcript: list | dict | Transcript) -> string", category = "transcript")]
fn transcript_render_visible_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let transcript = require_transcript(args, "transcript_render_visible")?;
    let rendered = match transcript.get("events") {
        Some(VmValue::List(events)) => events
            .iter()
            .filter_map(|event| {
                let dict = event.as_dict()?;
                if dict.get("visibility")?.display() != "public" {
                    return None;
                }
                let role = dict.get("role").map(VmValue::display).unwrap_or_default();
                let text = dict.get("text").map(VmValue::display).unwrap_or_default();
                (!text.is_empty()).then(|| format!("{role}: {text}"))
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    };
    Ok(VmValue::String(arcstr::ArcStr::from(rendered)))
}

#[harn_builtin(exposure = "pure", effects = [], sig = "transcript_render_full(transcript: list | dict | Transcript) -> string", category = "transcript")]
fn transcript_render_full_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let transcript = require_transcript(args, "transcript_render_full")?;
    let rendered = match transcript.get("events") {
        Some(VmValue::List(events)) => events
            .iter()
            .filter_map(|event| {
                let dict = event.as_dict()?;
                let role = dict.get("role").map(VmValue::display).unwrap_or_default();
                let visibility = dict
                    .get("visibility")
                    .map(VmValue::display)
                    .unwrap_or_default();
                let text = dict.get("text").map(VmValue::display).unwrap_or_default();
                Some(format!("[{visibility}] {role}: {text}"))
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    };
    Ok(VmValue::String(arcstr::ArcStr::from(rendered)))
}

#[harn_builtin(exposure = "pure", effects = [], sig = "transcript_export(transcript: list | dict | Transcript) -> string", category = "transcript")]
fn transcript_export_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let transcript = args.first().cloned().unwrap_or(VmValue::Nil);
    if !is_transcript_value(&transcript) {
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "transcript_export: argument must be a transcript",
        ))));
    }
    let json = serde_json::to_string_pretty(&vm_value_to_json(&transcript))
        .map_err(|error| VmError::Runtime(format!("transcript_export: {error}")))?;
    Ok(VmValue::String(arcstr::ArcStr::from(json)))
}

#[harn_builtin(exposure = "pure", effects = [], sig = "transcript_import(text: string) -> Transcript", category = "transcript")]
fn transcript_import_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let text = args.first().map(VmValue::display).unwrap_or_default();
    let json = serde_json::from_str(&text)
        .map_err(|error| VmError::Runtime(format!("transcript_import: {error}")))?;
    Ok(crate::stdlib::json_to_vm_value(&json))
}

/// Register conversation management builtins.
pub(crate) fn register_conversation_builtins(vm: &mut Vm) {
    register_builtin_defs(vm, CONVERSATION_BUILTINS);
    vm.register_builtin_def(&CONVERSATION_IMPL_DEF);
    vm.register_builtin_def(&TRANSCRIPT_ADD_ASSET_IMPL_DEF);
    vm.register_builtin_def(&TRANSCRIPT_RENDER_VISIBLE_IMPL_DEF);
    vm.register_builtin_def(&TRANSCRIPT_RENDER_FULL_IMPL_DEF);
    vm.register_builtin_def(&TRANSCRIPT_EXPORT_IMPL_DEF);
    vm.register_builtin_def(&TRANSCRIPT_IMPORT_IMPL_DEF);
    vm.register_builtin_def(&TRANSCRIPT_INJECT_REMINDER_BUILTIN_DEF);
    vm.register_builtin_def(&TRANSCRIPT_CLEAR_REMINDERS_BUILTIN_DEF);
}

fn transcript_state(transcript: &crate::value::DictMap) -> Option<&str> {
    transcript.get("state").and_then(|value| match value {
        VmValue::String(text) if !text.is_empty() => Some(text.as_ref()),
        _ => None,
    })
}

fn transcript_extra_events(transcript: &crate::value::DictMap) -> Vec<VmValue> {
    transcript
        .get("events")
        .and_then(|events| match events {
            VmValue::List(list) => Some(
                list.iter()
                    .filter(|event| {
                        event
                            .as_dict()
                            .and_then(|dict| dict.get("kind"))
                            .map(|value| value.display())
                            .is_some_and(|kind| kind != "message" && kind != "tool_result")
                    })
                    .cloned()
                    .collect(),
            ),
            _ => None,
        })
        .unwrap_or_default()
}

fn rebuild_transcript(
    transcript: &crate::value::DictMap,
    messages: Vec<VmValue>,
    summary: Option<String>,
    assets: Vec<VmValue>,
    extra_events: Vec<VmValue>,
    state: Option<&str>,
) -> VmValue {
    let preserved = transcript_extra_events(transcript);
    rebuild_transcript_with_preserved_events(
        transcript,
        messages,
        summary,
        assets,
        preserved,
        extra_events,
        state,
    )
}

fn rebuild_transcript_with_preserved_events(
    transcript: &crate::value::DictMap,
    messages: Vec<VmValue>,
    summary: Option<String>,
    assets: Vec<VmValue>,
    mut preserved: Vec<VmValue>,
    mut extra_events: Vec<VmValue>,
    state: Option<&str>,
) -> VmValue {
    preserved.append(&mut extra_events);
    new_transcript_with_events(
        transcript_id(transcript),
        messages,
        summary,
        transcript.get("metadata").cloned(),
        preserved,
        assets,
        state,
    )
}

#[harn_builtin(exposure = "privileged_wire", effects = ["state.mutate@arg0", "random.mutate@const=reminder-id", "observability.write@const=reminder-lifecycle"], sig = "__transcript_inject_reminder(transcript: list | dict | Transcript, options: dict) -> dict", category = "transcript")]
fn transcript_inject_reminder_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let context = "transcript.inject_reminder";
    let transcript = require_transcript(args, context)?;
    let options = require_reminder_options(args, 1, context)?;
    ensure_known_reminder_keys(context, options, INJECT_REMINDER_KEYS)?;
    let reminder = parse_inject_reminder_options(options, context)?;

    let mut preserved = transcript_extra_events(transcript);
    let mut deduped_reminder_ids = Vec::new();
    if let Some(dedupe_key) = reminder.dedupe_key.as_deref() {
        preserved.retain(|event| {
            let dropped_id = reminder_payload(event).and_then(|payload| {
                let key_matches = reminder_string_field(payload, "dedupe_key")
                    .is_some_and(|key| key == dedupe_key);
                if key_matches {
                    Some(reminder_string_field(payload, "id").unwrap_or_default())
                } else {
                    None
                }
            });
            if let Some(id) = dropped_id {
                deduped_reminder_ids.push(id);
                false
            } else {
                true
            }
        });
    }

    let reminder_id = reminder.id.clone();
    let reminder_event = transcript_reminder_event(&reminder);
    let transcript_id = transcript_id(transcript);
    let next = rebuild_transcript_with_preserved_events(
        transcript,
        transcript_message_list(transcript)?,
        transcript_summary_text(transcript),
        transcript_asset_list(transcript)?,
        preserved,
        vec![reminder_event],
        transcript_state(transcript),
    );

    if !deduped_reminder_ids.is_empty() {
        let dropped_count = deduped_reminder_ids.len();
        emit_reminder_lifecycle_event(
            REMINDER_DEDUPED_EVENT_KIND,
            serde_json::json!({
                "transcript_id": &transcript_id,
                "reminder_id": &reminder_id,
                "replacing_id": &reminder_id,
                "replaced_id": deduped_reminder_ids.first(),
                "replaced_ids": &deduped_reminder_ids,
                "dedupe_key": &reminder.dedupe_key,
                "dropped_reminder_ids": &deduped_reminder_ids,
                "dropped_count": dropped_count,
            }),
        );
    }

    emit_reminder_lifecycle_event(
        REMINDER_INJECTED_EVENT_KIND,
        reminder_lifecycle_payload(transcript_id.as_deref(), &reminder),
    );

    Ok(VmValue::dict(crate::value::DictMap::from_iter([
        (crate::value::intern_key("transcript"), next),
        (
            crate::value::intern_key("reminder_id"),
            VmValue::String(arcstr::ArcStr::from(reminder_id)),
        ),
        (
            crate::value::intern_key("deduped_count"),
            VmValue::Int(deduped_reminder_ids.len() as i64),
        ),
    ])))
}

#[harn_builtin(exposure = "privileged_wire", effects = ["state.mutate@arg0", "observability.write@const=reminder-lifecycle"], sig = "__transcript_clear_reminders(transcript: list | dict | Transcript, selector: dict) -> dict", category = "transcript")]
fn transcript_clear_reminders_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let context = "transcript.clear_reminders";
    let transcript = require_transcript(args, context)?;
    let options = require_reminder_options(args, 1, context)?;
    ensure_known_reminder_keys(context, options, CLEAR_REMINDER_KEYS)?;
    let selector = parse_clear_reminder_selector(options, context)?;

    let mut removed_count = 0_i64;
    let mut removed = Vec::new();
    let mut preserved = Vec::new();
    for event in transcript_extra_events(transcript) {
        if reminder_payload(&event).is_some_and(|payload| selector.matches(payload)) {
            removed_count += 1;
            if let Some(reminder) = reminder_from_event(&event) {
                removed.push(reminder);
            }
        } else {
            preserved.push(event);
        }
    }

    let next = rebuild_transcript_with_preserved_events(
        transcript,
        transcript_message_list(transcript)?,
        transcript_summary_text(transcript),
        transcript_asset_list(transcript)?,
        preserved,
        Vec::new(),
        transcript_state(transcript),
    );

    let transcript_id = transcript_id(transcript);
    for reminder in &removed {
        let mut payload = reminder_lifecycle_payload(transcript_id.as_deref(), reminder);
        if let Some(obj) = payload.as_object_mut() {
            obj.insert(
                "reason".to_string(),
                serde_json::Value::String("cleared".to_string()),
            );
        }
        emit_reminder_lifecycle_event(REMINDER_EXPIRED_EVENT_KIND, payload);
    }

    Ok(VmValue::dict(crate::value::DictMap::from_iter([
        (crate::value::intern_key("transcript"), next),
        (
            crate::value::intern_key("removed_count"),
            VmValue::Int(removed_count),
        ),
    ])))
}

fn require_reminder_options<'a>(
    args: &'a [VmValue],
    index: usize,
    context: &str,
) -> Result<&'a crate::value::DictMap, VmError> {
    match args.get(index) {
        Some(VmValue::Dict(dict)) => Ok(dict),
        Some(other) => Err(reminder_code_error(
            context,
            Code::ReminderUnknownOption,
            format!("options must be a dict, got {}", other.type_name()),
        )),
        None => Err(reminder_code_error(
            context,
            Code::ReminderUnknownOption,
            "options are required",
        )),
    }
}

pub(crate) fn ensure_known_reminder_keys(
    context: &str,
    options: &crate::value::DictMap,
    allowed: &[&str],
) -> Result<(), VmError> {
    let unknown = options
        .keys()
        .filter(|key| !allowed.contains(&key.as_str()))
        .map(|key| key.as_str())
        .collect::<Vec<_>>();
    if unknown.is_empty() {
        Ok(())
    } else {
        Err(reminder_code_error(
            context,
            Code::ReminderUnknownOption,
            format!("unknown option(s): {}", unknown.join(", ")),
        ))
    }
}

pub(crate) fn parse_inject_reminder_options(
    options: &crate::value::DictMap,
    context: &str,
) -> Result<SystemReminder, VmError> {
    Ok(SystemReminder {
        id: uuid::Uuid::now_v7().to_string(),
        tags: reminder_tags(options, context)?,
        dedupe_key: optional_reminder_string(options, "dedupe_key", context)?,
        ttl_turns: optional_reminder_ttl(options, context)?,
        preserve_on_compact: optional_reminder_bool(options, "preserve_on_compact", context)?
            .unwrap_or(false),
        propagate: optional_reminder_propagate(options, context)?
            .unwrap_or(ReminderPropagate::Session),
        role_hint: optional_reminder_role_hint(options, context)?
            .unwrap_or(ReminderRoleHint::System),
        authority: optional_reminder_authority(options, context)?.unwrap_or_default(),
        source: ReminderSource::InPipeline,
        body: required_reminder_string(options, "body", context)?,
        fired_at_turn: 0,
        originating_agent_id: None,
    })
}

#[derive(Debug, Default)]
struct ClearReminderSelector {
    id: Option<String>,
    tag: Option<String>,
    dedupe_key: Option<String>,
}

impl ClearReminderSelector {
    fn matches(&self, reminder: &crate::value::DictMap) -> bool {
        if let Some(expected) = self.id.as_deref() {
            if reminder_string_field(reminder, "id").as_deref() != Some(expected) {
                return false;
            }
        }
        if let Some(expected) = self.dedupe_key.as_deref() {
            if reminder_string_field(reminder, "dedupe_key").as_deref() != Some(expected) {
                return false;
            }
        }
        if let Some(expected) = self.tag.as_deref() {
            let Some(VmValue::List(tags)) = reminder.get("tags") else {
                return false;
            };
            if !tags.iter().any(|tag| tag.display() == expected) {
                return false;
            }
        }
        true
    }
}

fn parse_clear_reminder_selector(
    options: &crate::value::DictMap,
    context: &str,
) -> Result<ClearReminderSelector, VmError> {
    let selector = ClearReminderSelector {
        id: optional_reminder_string(options, "id", context)?,
        tag: optional_reminder_string(options, "tag", context)?,
        dedupe_key: optional_reminder_string(options, "dedupe_key", context)?,
    };
    if selector.id.is_none() && selector.tag.is_none() && selector.dedupe_key.is_none() {
        return Err(reminder_code_error(
            context,
            Code::ReminderUnknownOption,
            "at least one of id, tag, or dedupe_key is required",
        ));
    }
    Ok(selector)
}

fn required_reminder_string(
    options: &crate::value::DictMap,
    key: &str,
    context: &str,
) -> Result<String, VmError> {
    match options.get(key) {
        Some(VmValue::String(value)) if !value.trim().is_empty() => Ok(value.to_string()),
        Some(VmValue::String(_)) | None | Some(VmValue::Nil) => Err(reminder_code_error(
            context,
            Code::ReminderUnknownOption,
            format!("`{key}` must be a non-empty string"),
        )),
        Some(other) => Err(reminder_code_error(
            context,
            Code::ReminderUnknownOption,
            format!("`{key}` must be a string, got {}", other.type_name()),
        )),
    }
}

fn optional_reminder_string(
    options: &crate::value::DictMap,
    key: &str,
    context: &str,
) -> Result<Option<String>, VmError> {
    match options.get(key) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::String(value)) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(trimmed.to_string()))
            }
        }
        Some(other) => Err(reminder_code_error(
            context,
            Code::ReminderUnknownOption,
            format!("`{key}` must be a string or nil, got {}", other.type_name()),
        )),
    }
}

fn reminder_tags(options: &crate::value::DictMap, context: &str) -> Result<Vec<String>, VmError> {
    match options.get("tags") {
        None | Some(VmValue::Nil) => Ok(Vec::new()),
        Some(VmValue::List(values)) => {
            let mut tags = Vec::new();
            for value in values.iter() {
                let VmValue::String(tag) = value else {
                    return Err(reminder_code_error(
                        context,
                        Code::ReminderUnknownOption,
                        format!("`tags` entries must be strings, got {}", value.type_name()),
                    ));
                };
                let trimmed = tag.trim();
                if trimmed.is_empty() {
                    return Err(reminder_code_error(
                        context,
                        Code::ReminderUnknownOption,
                        "`tags` entries must be non-empty strings",
                    ));
                }
                if !tags.iter().any(|existing| existing == trimmed) {
                    tags.push(trimmed.to_string());
                }
            }
            Ok(tags)
        }
        Some(other) => Err(reminder_code_error(
            context,
            Code::ReminderUnknownOption,
            format!("`tags` must be a list or nil, got {}", other.type_name()),
        )),
    }
}

fn optional_reminder_bool(
    options: &crate::value::DictMap,
    key: &str,
    context: &str,
) -> Result<Option<bool>, VmError> {
    match options.get(key) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::Bool(value)) => Ok(Some(*value)),
        Some(other) => Err(reminder_code_error(
            context,
            Code::ReminderUnknownOption,
            format!("`{key}` must be a bool or nil, got {}", other.type_name()),
        )),
    }
}

fn optional_reminder_ttl(
    options: &crate::value::DictMap,
    context: &str,
) -> Result<Option<i64>, VmError> {
    match options.get("ttl_turns") {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::Int(value)) if *value > 0 => Ok(Some(*value)),
        Some(VmValue::Int(_)) => Err(reminder_code_error(
            context,
            Code::ReminderUnknownOption,
            "`ttl_turns` must be > 0",
        )),
        Some(other) => Err(reminder_code_error(
            context,
            Code::ReminderUnknownOption,
            format!(
                "`ttl_turns` must be an int or nil, got {}",
                other.type_name()
            ),
        )),
    }
}

fn optional_reminder_propagate(
    options: &crate::value::DictMap,
    context: &str,
) -> Result<Option<ReminderPropagate>, VmError> {
    optional_reminder_string(options, "propagate", context)?
        .map(|value| match value.as_str() {
            "all" => Ok(ReminderPropagate::All),
            "session" => Ok(ReminderPropagate::Session),
            "none" => Ok(ReminderPropagate::None),
            _ => Err(reminder_code_error(
                context,
                Code::ReminderUnknownPropagate,
                "`propagate` must be one of all, session, or none",
            )),
        })
        .transpose()
}

fn optional_reminder_role_hint(
    options: &crate::value::DictMap,
    context: &str,
) -> Result<Option<ReminderRoleHint>, VmError> {
    optional_reminder_string(options, "role_hint", context)?
        .map(|value| match value.as_str() {
            "system" => Ok(ReminderRoleHint::System),
            "developer" => Ok(ReminderRoleHint::Developer),
            "user_block" => Ok(ReminderRoleHint::UserBlock),
            "ephemeral_cache" => Ok(ReminderRoleHint::EphemeralCache),
            _ => Err(reminder_code_error(
                context,
                Code::ReminderUnknownOption,
                "`role_hint` must be one of system, developer, user_block, or ephemeral_cache",
            )),
        })
        .transpose()
}

fn optional_reminder_authority(
    options: &crate::value::DictMap,
    context: &str,
) -> Result<Option<DirectiveAuthority>, VmError> {
    optional_reminder_string(options, "authority", context)?
        .map(|value| match value.as_str() {
            "contract" => Ok(DirectiveAuthority::Contract),
            "corrective" => Ok(DirectiveAuthority::Corrective),
            "advisory" => Ok(DirectiveAuthority::Advisory),
            _ => Err(reminder_code_error(
                context,
                Code::ReminderUnknownOption,
                "`authority` must be one of contract, corrective, or advisory",
            )),
        })
        .transpose()
}

fn reminder_payload(event: &VmValue) -> Option<&crate::value::DictMap> {
    let event = event.as_dict()?;
    if event.get("kind").map(|value| value.display()).as_deref() != Some(SYSTEM_REMINDER_EVENT_KIND)
    {
        return None;
    }
    event.get("reminder").and_then(VmValue::as_dict)
}

fn reminder_string_field(reminder: &crate::value::DictMap, key: &str) -> Option<String> {
    match reminder.get(key) {
        Some(VmValue::String(value)) if !value.is_empty() => Some(value.to_string()),
        _ => None,
    }
}

fn reminder_error(context: &str, message: impl Into<String>) -> VmError {
    VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
        "{context}: {}",
        message.into()
    ))))
}

fn reminder_code_error(context: &str, code: Code, message: impl Into<String>) -> VmError {
    reminder_error(context, format!("{}: {}", code.as_str(), message.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vm_string(value: &str) -> VmValue {
        VmValue::String(arcstr::ArcStr::from(value))
    }

    fn dict(entries: Vec<(&str, VmValue)>) -> VmValue {
        VmValue::dict(
            entries
                .into_iter()
                .map(|(key, value)| (crate::value::intern_key(key), value))
                .collect::<crate::value::DictMap>(),
        )
    }

    fn strings(values: &[&str]) -> VmValue {
        VmValue::List(std::sync::Arc::new(
            values.iter().map(|value| vm_string(value)).collect(),
        ))
    }

    fn result_transcript(value: &VmValue) -> VmValue {
        value
            .as_dict()
            .and_then(|dict| dict.get("transcript"))
            .cloned()
            .expect("result transcript")
    }

    fn system_reminder_events(transcript: &VmValue) -> Vec<VmValue> {
        transcript
            .as_dict()
            .and_then(|dict| dict.get("events"))
            .and_then(|events| match events {
                VmValue::List(values) => Some(values),
                _ => None,
            })
            .expect("events list")
            .iter()
            .filter(|event| reminder_payload(event).is_some())
            .cloned()
            .collect()
    }

    #[test]
    fn inject_replaces_pending_reminder_with_same_dedupe_key() {
        let base = new_transcript_with(None, Vec::new(), None, None);
        let first = transcript_inject_reminder_builtin(
            &[
                base,
                dict(vec![
                    ("body", vm_string("first")),
                    ("tags", strings(&["context"])),
                    ("dedupe_key", vm_string("context")),
                ]),
            ],
            &mut String::new(),
        )
        .expect("first inject");
        let second = transcript_inject_reminder_builtin(
            &[
                result_transcript(&first),
                dict(vec![
                    ("body", vm_string("second")),
                    ("tags", strings(&["context"])),
                    ("dedupe_key", vm_string("context")),
                ]),
            ],
            &mut String::new(),
        )
        .expect("second inject");

        let second_dict = second.as_dict().expect("result dict");
        assert_eq!(
            second_dict.get("deduped_count").and_then(VmValue::as_int),
            Some(1)
        );
        let reminders = system_reminder_events(
            second_dict
                .get("transcript")
                .expect("transformed transcript in result"),
        );
        assert_eq!(reminders.len(), 1);
        let payload = reminder_payload(&reminders[0]).expect("reminder payload");
        assert_eq!(
            reminder_string_field(payload, "body").as_deref(),
            Some("second")
        );
    }

    #[test]
    fn clear_reminders_filters_by_tag() {
        let base = new_transcript_with(None, Vec::new(), None, None);
        let first = transcript_inject_reminder_builtin(
            &[
                base,
                dict(vec![
                    ("body", vm_string("keep")),
                    ("tags", strings(&["keep"])),
                ]),
            ],
            &mut String::new(),
        )
        .expect("first inject");
        let second = transcript_inject_reminder_builtin(
            &[
                result_transcript(&first),
                dict(vec![
                    ("body", vm_string("drop")),
                    ("tags", strings(&["drop"])),
                ]),
            ],
            &mut String::new(),
        )
        .expect("second inject");
        let cleared = transcript_clear_reminders_builtin(
            &[
                result_transcript(&second),
                dict(vec![("tag", vm_string("drop"))]),
            ],
            &mut String::new(),
        )
        .expect("clear reminders");
        let cleared_dict = cleared.as_dict().expect("result dict");
        assert_eq!(
            cleared_dict.get("removed_count").and_then(VmValue::as_int),
            Some(1)
        );
        let reminders = system_reminder_events(
            cleared_dict
                .get("transcript")
                .expect("transformed transcript in result"),
        );
        assert_eq!(reminders.len(), 1);
        let payload = reminder_payload(&reminders[0]).expect("reminder payload");
        assert_eq!(
            reminder_string_field(payload, "body").as_deref(),
            Some("keep")
        );
    }

    #[test]
    fn unknown_reminder_option_reports_key() {
        let base = new_transcript_with(None, Vec::new(), None, None);
        let err = transcript_inject_reminder_builtin(
            &[
                base,
                dict(vec![
                    ("body", vm_string("hello")),
                    ("typo_key", VmValue::Bool(true)),
                ]),
            ],
            &mut String::new(),
        )
        .expect_err("unknown key should fail");
        match err {
            VmError::Thrown(VmValue::String(message)) => {
                assert!(message.contains(Code::ReminderUnknownOption.as_str()));
                assert!(message.contains("typo_key"), "{message}");
            }
            other => panic!("expected thrown reminder error, got {other:?}"),
        }
    }

    #[test]
    fn invalid_reminder_option_type_reports_code() {
        let base = new_transcript_with(None, Vec::new(), None, None);
        let err = transcript_inject_reminder_builtin(
            &[base, dict(vec![("body", VmValue::Int(1))])],
            &mut String::new(),
        )
        .expect_err("invalid body type should fail");
        match err {
            VmError::Thrown(VmValue::String(message)) => {
                assert!(message.contains(Code::ReminderUnknownOption.as_str()));
                assert!(message.contains("body"), "{message}");
            }
            other => panic!("expected thrown reminder error, got {other:?}"),
        }
    }

    #[test]
    fn invalid_reminder_authority_reports_allowed_contract() {
        let base = new_transcript_with(None, Vec::new(), None, None);
        let err = transcript_inject_reminder_builtin(
            &[
                base,
                dict(vec![
                    ("body", vm_string("hello")),
                    ("authority", vm_string("urgent")),
                ]),
            ],
            &mut String::new(),
        )
        .expect_err("unknown authority should fail");
        match err {
            VmError::Thrown(VmValue::String(message)) => {
                assert!(message.contains(Code::ReminderUnknownOption.as_str()));
                assert!(
                    message.contains("contract, corrective, or advisory"),
                    "{message}"
                );
            }
            other => panic!("expected thrown reminder error, got {other:?}"),
        }
    }

    #[test]
    fn unknown_reminder_propagate_reports_specific_code() {
        let base = new_transcript_with(None, Vec::new(), None, None);
        let err = transcript_inject_reminder_builtin(
            &[
                base,
                dict(vec![
                    ("body", vm_string("hello")),
                    ("propagate", vm_string("workspace")),
                ]),
            ],
            &mut String::new(),
        )
        .expect_err("unknown propagate should fail");
        match err {
            VmError::Thrown(VmValue::String(message)) => {
                assert!(message.contains(Code::ReminderUnknownPropagate.as_str()));
                assert!(message.contains("propagate"), "{message}");
            }
            other => panic!("expected thrown reminder error, got {other:?}"),
        }
    }
}
