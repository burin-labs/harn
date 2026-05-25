use std::collections::BTreeMap;
use std::rc::Rc;

use crate::value::{VmError, VmValue};
use crate::vm::{Vm, VmBuiltinArity, VmBuiltinMetadata};
use harn_parser::diagnostic_codes::Code;

use super::helpers::{
    emit_reminder_lifecycle_event, extract_llm_options, is_transcript_value, new_transcript_with,
    new_transcript_with_events, normalize_transcript_asset, reminder_from_event,
    reminder_lifecycle_payload, transcript_asset_list, transcript_drain_decision_event_from_value,
    transcript_event, transcript_id, transcript_message_list, transcript_reminder_event,
    transcript_reminder_event_from_value, transcript_resumption_event_from_value,
    transcript_summary_text, transcript_suspension_event_from_value, vm_add_role_message,
    vm_message_value, vm_value_to_json, ReminderPropagate, ReminderRoleHint, ReminderSource,
    SystemReminder, REMINDER_DEDUPED_EVENT_KIND, REMINDER_EXPIRED_EVENT_KIND,
    REMINDER_INJECTED_EVENT_KIND, SYSTEM_REMINDER_EVENT_KIND,
};

const INJECT_REMINDER_KEYS: &[&str] = &[
    "body",
    "tags",
    "dedupe_key",
    "ttl_turns",
    "preserve_on_compact",
    "propagate",
    "role_hint",
];
const CLEAR_REMINDER_KEYS: &[&str] = &["id", "tag", "dedupe_key"];

const CONVERSATION_BUILTIN_NAMES: &[&str] = &[
    "conversation",
    "transcript",
    "transcript.inject_reminder",
    "transcript.clear_reminders",
    "transcript_from_messages",
    "transcript_messages",
    "transcript_assets",
    "transcript_add_asset",
    "transcript_events",
    "transcript_reminder_event",
    "transcript_suspension_event",
    "transcript_resumption_event",
    "transcript_drain_decision_event",
    "transcript_summary",
    "transcript_id",
    "transcript_render_visible",
    "transcript_render_full",
    "transcript_summarize",
    "transcript_export",
    "transcript_import",
    "transcript_fork",
    "transcript_reset",
    "transcript_archive",
    "transcript_abandon",
    "transcript_resume",
    "add_message",
    "add_user",
    "add_assistant",
    "add_system",
    "add_tool_result",
];

pub(crate) fn register_deferred_conversation_builtins(vm: &mut Vm, registrar: fn(&mut Vm)) {
    for name in CONVERSATION_BUILTIN_NAMES {
        vm.register_deferred_builtin(name, registrar);
    }
}

/// Extract and validate a transcript dict from the first argument.
fn require_transcript<'a>(
    args: &'a [VmValue],
    context: &str,
) -> Result<&'a BTreeMap<String, VmValue>, VmError> {
    match args.first() {
        Some(VmValue::Dict(d))
            if d.get("_type").map(|v| v.display()).as_deref() == Some("transcript") =>
        {
            Ok(d)
        }
        _ => Err(VmError::Thrown(VmValue::String(Rc::from(format!(
            "{context}: argument must be a transcript"
        ))))),
    }
}

/// Register conversation management builtins.
pub(crate) fn register_conversation_builtins(vm: &mut Vm) {
    vm.register_builtin("conversation", |_args, _out| {
        Ok(VmValue::List(Rc::new(Vec::new())))
    });

    vm.register_builtin("transcript", |args, _out| {
        let metadata = args.first().cloned();
        Ok(new_transcript_with(None, Vec::new(), None, metadata))
    });

    vm.register_builtin_with_metadata(
        VmBuiltinMetadata::sync_static("transcript.inject_reminder")
            .signature_static("transcript.inject_reminder(transcript, options) -> dict")
            .arity(VmBuiltinArity::Exact(2))
            .category_static("transcript")
            .doc_static(
                "Inject a pending system reminder and return {transcript, reminder_id, deduped_count}.",
            ),
        |args, _out| transcript_inject_reminder_builtin(args),
    );

    vm.register_builtin_with_metadata(
        VmBuiltinMetadata::sync_static("transcript.clear_reminders")
            .signature_static("transcript.clear_reminders(transcript, selector) -> dict")
            .arity(VmBuiltinArity::Exact(2))
            .category_static("transcript")
            .doc_static(
                "Remove pending system reminders by id, tag, and/or dedupe_key and return {transcript, removed_count}.",
            ),
        |args, _out| transcript_clear_reminders_builtin(args),
    );

    vm.register_builtin("transcript_from_messages", |args, _out| {
        let messages = match args.first() {
            Some(VmValue::List(list)) => (**list).clone(),
            Some(VmValue::Dict(d)) if is_transcript_value(&VmValue::Dict(d.clone())) => {
                transcript_message_list(d)?
            }
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "transcript_from_messages: argument must be a message list or transcript",
                ))));
            }
        };
        Ok(new_transcript_with(None, messages, None, None))
    });

    vm.register_builtin("transcript_messages", |args, _out| {
        let transcript = require_transcript(args, "transcript_messages")?;
        Ok(VmValue::List(Rc::new(transcript_message_list(transcript)?)))
    });

    vm.register_builtin("transcript_assets", |args, _out| {
        let transcript = require_transcript(args, "transcript_assets")?;
        Ok(VmValue::List(Rc::new(transcript_asset_list(transcript)?)))
    });

    vm.register_builtin("transcript_add_asset", |args, _out| {
        let transcript = require_transcript(args, "transcript_add_asset")?;
        let asset_value = args.get(1).cloned().ok_or_else(|| {
            VmError::Thrown(VmValue::String(Rc::from(
                "transcript_add_asset: missing asset",
            )))
        })?;
        let normalized = normalize_transcript_asset(&asset_value);
        let asset_id = normalized
            .as_dict()
            .and_then(|dict| dict.get("id"))
            .map(|value| value.display())
            .unwrap_or_default();
        let mut assets = transcript_asset_list(transcript)?;
        assets.retain(|asset| {
            asset
                .as_dict()
                .and_then(|dict| dict.get("id"))
                .map(|value| value.display())
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
    });

    vm.register_builtin("transcript_events", |args, _out| {
        let transcript = require_transcript(args, "transcript_events")?;
        Ok(transcript
            .get("events")
            .cloned()
            .unwrap_or_else(|| VmValue::List(Rc::new(Vec::new()))))
    });

    // Build a normalized `system_reminder` transcript event from a dict
    // describing the reminder lifecycle (body/tags/dedupe_key/ttl_turns/
    // preserve_on_compact/propagate/role_hint/source/fired_at_turn).
    // Defaults are applied per spec when fields are omitted. Returns the
    // canonical event envelope so callers can pass the result straight
    // to `agent_session_append_event` or splice it into an
    // event list manually.
    vm.register_builtin("transcript_reminder_event", |args, _out| {
        let value = args.first().cloned().unwrap_or(VmValue::Nil);
        Ok(transcript_reminder_event_from_value(&value))
    });

    vm.register_builtin("transcript_suspension_event", |args, _out| {
        let value = args.first().cloned().unwrap_or(VmValue::Nil);
        Ok(transcript_suspension_event_from_value(&value))
    });

    vm.register_builtin("transcript_resumption_event", |args, _out| {
        let value = args.first().cloned().unwrap_or(VmValue::Nil);
        Ok(transcript_resumption_event_from_value(&value))
    });

    vm.register_builtin("transcript_drain_decision_event", |args, _out| {
        let value = args.first().cloned().unwrap_or(VmValue::Nil);
        Ok(transcript_drain_decision_event_from_value(&value))
    });

    vm.register_builtin("transcript_summary", |args, _out| {
        let transcript = require_transcript(args, "transcript_summary")?;
        Ok(transcript.get("summary").cloned().unwrap_or(VmValue::Nil))
    });

    vm.register_builtin("transcript_id", |args, _out| {
        let transcript = require_transcript(args, "transcript_id")?;
        Ok(VmValue::String(Rc::from(
            transcript_id(transcript).unwrap_or_default(),
        )))
    });

    vm.register_builtin("transcript_render_visible", |args, _out| {
        let transcript = require_transcript(args, "transcript_render_visible")?;
        let rendered = match transcript.get("events") {
            Some(VmValue::List(events)) => events
                .iter()
                .filter_map(|event| {
                    let dict = event.as_dict()?;
                    let visibility = dict.get("visibility")?.display();
                    if visibility != "public" {
                        return None;
                    }
                    let role = dict
                        .get("role")
                        .map(|value| value.display())
                        .unwrap_or_default();
                    let text = dict
                        .get("text")
                        .map(|value| value.display())
                        .unwrap_or_default();
                    if text.is_empty() {
                        None
                    } else {
                        Some(format!("{role}: {text}"))
                    }
                })
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        };
        Ok(VmValue::String(Rc::from(rendered)))
    });

    vm.register_builtin("transcript_render_full", |args, _out| {
        let transcript = require_transcript(args, "transcript_render_full")?;
        let rendered = match transcript.get("events") {
            Some(VmValue::List(events)) => events
                .iter()
                .filter_map(|event| {
                    let dict = event.as_dict()?;
                    let role = dict
                        .get("role")
                        .map(|value| value.display())
                        .unwrap_or_default();
                    let visibility = dict
                        .get("visibility")
                        .map(|value| value.display())
                        .unwrap_or_default();
                    let text = dict
                        .get("text")
                        .map(|value| value.display())
                        .unwrap_or_default();
                    Some(format!("[{visibility}] {role}: {text}"))
                })
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        };
        Ok(VmValue::String(Rc::from(rendered)))
    });

    vm.register_builtin("transcript_export", |args, _out| {
        let transcript = args.first().cloned().unwrap_or(VmValue::Nil);
        if !is_transcript_value(&transcript) {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "transcript_export: argument must be a transcript",
            ))));
        }
        let json = serde_json::to_string_pretty(&vm_value_to_json(&transcript))
            .map_err(|e| VmError::Runtime(format!("transcript_export: {e}")))?;
        Ok(VmValue::String(Rc::from(json)))
    });

    vm.register_builtin("transcript_import", |args, _out| {
        let text = args.first().map(|a| a.display()).unwrap_or_default();
        let json: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| VmError::Runtime(format!("transcript_import: {e}")))?;
        Ok(crate::stdlib::json_to_vm_value(&json))
    });

    vm.register_builtin("transcript_fork", |args, _out| {
        let transcript = require_transcript(args, "transcript_fork")?;
        let options = args.get(1).and_then(|v| v.as_dict());
        let retain_messages = options
            .and_then(|d| d.get("retain_messages"))
            .map(|v| v.is_truthy())
            .unwrap_or(true);
        let retain_summary = options
            .and_then(|d| d.get("retain_summary"))
            .map(|v| v.is_truthy())
            .unwrap_or(true);
        let messages = if retain_messages {
            transcript_message_list(transcript)?
        } else {
            Vec::new()
        };
        let summary = if retain_summary {
            transcript_summary_text(transcript)
        } else {
            None
        };
        Ok(rebuild_transcript(
            transcript,
            messages,
            summary,
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
    });

    vm.register_builtin("transcript_reset", |args, _out| {
        let metadata = args
            .first()
            .and_then(|value| value.as_dict())
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
    });

    vm.register_builtin("transcript_archive", |args, _out| {
        let transcript = require_transcript(args, "transcript_archive")?;
        let messages = transcript_message_list(transcript)?;
        Ok(rebuild_transcript(
            transcript,
            messages,
            transcript_summary_text(transcript),
            transcript_asset_list(transcript)?,
            vec![transcript_event(
                "transcript_archive",
                "system",
                "internal",
                "transcript archived",
                None,
            )],
            Some("archived"),
        ))
    });

    vm.register_builtin("transcript_abandon", |args, _out| {
        let transcript = require_transcript(args, "transcript_abandon")?;
        Ok(rebuild_transcript(
            transcript,
            transcript_message_list(transcript)?,
            transcript_summary_text(transcript),
            transcript_asset_list(transcript)?,
            vec![transcript_event(
                "transcript_abandon",
                "system",
                "internal",
                "transcript abandoned",
                None,
            )],
            Some("abandoned"),
        ))
    });

    vm.register_builtin("transcript_resume", |args, _out| {
        let transcript = require_transcript(args, "transcript_resume")?;
        Ok(rebuild_transcript(
            transcript,
            transcript_message_list(transcript)?,
            transcript_summary_text(transcript),
            transcript_asset_list(transcript)?,
            vec![transcript_event(
                "transcript_resume",
                "system",
                "internal",
                "transcript resumed",
                None,
            )],
            Some("active"),
        ))
    });

    vm.register_async_builtin("transcript_summarize", |args| async move {
        let transcript = require_transcript(&args, "transcript_summarize")?;
        let mut opts = extract_llm_options(&[
            VmValue::String(Rc::from("")),
            VmValue::Nil,
            args.get(1).cloned().unwrap_or(VmValue::Nil),
        ])?;
        let keep_last = args
            .get(1)
            .and_then(|v| v.as_dict())
            .and_then(|d| d.get("keep_last"))
            .and_then(|v| v.as_int())
            .unwrap_or(6)
            .max(0) as usize;
        let prompt_override = args
            .get(1)
            .and_then(|v| v.as_dict())
            .and_then(|d| d.get("prompt"))
            .map(|v| v.display())
            .unwrap_or_default();

        let messages = transcript_message_list(transcript)?;
        let formatted = messages
            .iter()
            .map(|msg| {
                let dict = msg.as_dict();
                let role = dict
                    .and_then(|d| d.get("role"))
                    .map(|v| v.display())
                    .unwrap_or_else(|| "user".to_string());
                let content = dict
                    .and_then(|d| d.get("content"))
                    .map(|v| v.display())
                    .unwrap_or_default();
                format!("{}: {}", role.to_uppercase(), content)
            })
            .collect::<Vec<_>>()
            .join("\n");

        let mut bindings = BTreeMap::new();
        bindings.insert(
            "prompt".to_string(),
            VmValue::String(Rc::from(prompt_override)),
        );
        bindings.insert(
            "formatted".to_string(),
            VmValue::String(Rc::from(formatted)),
        );
        let user_message = crate::stdlib::template::render_stdlib_prompt_asset(
            "llm/prompts/transcript_summarize_user.harn.prompt",
            Some(&bindings),
        )?;
        opts.messages = vec![serde_json::json!({
            "role": "user",
            "content": user_message,
        })];

        let result = super::api::vm_call_llm_full(&opts).await?;
        let retained = messages
            .into_iter()
            .rev()
            .take(keep_last)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>();
        let archived_count = transcript_message_list(transcript)?
            .len()
            .saturating_sub(retained.len());
        let mut compacted = match rebuild_transcript(
            transcript,
            retained,
            Some(result.text),
            transcript_asset_list(transcript)?,
            Vec::new(),
            transcript_state(transcript),
        ) {
            VmValue::Dict(d) => (*d).clone(),
            _ => BTreeMap::new(),
        };
        compacted.insert(
            "archived_messages".to_string(),
            VmValue::Int(archived_count as i64),
        );
        Ok(VmValue::Dict(Rc::new(compacted)))
    });

    vm.register_builtin("add_message", |args, _out| match args.first() {
        Some(VmValue::List(list)) => {
            let role = args.get(1).map(|a| a.display()).unwrap_or_default();
            let mut new_messages = (**list).clone();
            new_messages.push(vm_message_value(
                &role,
                args.get(2)
                    .cloned()
                    .unwrap_or_else(|| VmValue::String(Rc::from(""))),
            ));
            Ok(VmValue::List(Rc::new(new_messages)))
        }
        Some(VmValue::Dict(d))
            if d.get("_type").map(|v| v.display()).as_deref() == Some("transcript") =>
        {
            let role = args.get(1).map(|a| a.display()).unwrap_or_default();
            let mut new_messages = transcript_message_list(d)?;
            new_messages.push(vm_message_value(
                &role,
                args.get(2)
                    .cloned()
                    .unwrap_or_else(|| VmValue::String(Rc::from(""))),
            ));
            Ok(rebuild_transcript(
                d,
                new_messages,
                transcript_summary_text(d),
                transcript_asset_list(d)?,
                Vec::new(),
                transcript_state(d),
            ))
        }
        _ => Err(VmError::Thrown(VmValue::String(Rc::from(
            "add_message: first argument must be a message list or transcript",
        )))),
    });

    vm.register_builtin("add_user", |args, _out| vm_add_role_message(args, "user"));

    vm.register_builtin("add_assistant", |args, _out| {
        vm_add_role_message(args, "assistant")
    });

    vm.register_builtin("add_system", |args, _out| {
        vm_add_role_message(args, "system")
    });

    vm.register_builtin("add_tool_result", |args, _out| match args.first() {
        Some(VmValue::List(list)) => {
            let tool_use_id = args.get(1).map(|a| a.display()).unwrap_or_default();
            let result_content = args.get(2).map(|a| a.display()).unwrap_or_default();
            let mut msg = BTreeMap::new();
            msg.insert("role".to_string(), VmValue::String(Rc::from("tool_result")));
            msg.insert(
                "tool_use_id".to_string(),
                VmValue::String(Rc::from(tool_use_id)),
            );
            msg.insert(
                "content".to_string(),
                VmValue::String(Rc::from(result_content)),
            );
            let mut new_messages = (**list).clone();
            new_messages.push(VmValue::Dict(Rc::new(msg)));
            Ok(VmValue::List(Rc::new(new_messages)))
        }
        Some(VmValue::Dict(d))
            if d.get("_type").map(|v| v.display()).as_deref() == Some("transcript") =>
        {
            let tool_use_id = args.get(1).map(|a| a.display()).unwrap_or_default();
            let result_content = args.get(2).map(|a| a.display()).unwrap_or_default();
            let mut msg = BTreeMap::new();
            msg.insert("role".to_string(), VmValue::String(Rc::from("tool_result")));
            msg.insert(
                "tool_use_id".to_string(),
                VmValue::String(Rc::from(tool_use_id)),
            );
            msg.insert(
                "content".to_string(),
                VmValue::String(Rc::from(result_content)),
            );
            let mut new_messages = transcript_message_list(d)?;
            new_messages.push(VmValue::Dict(Rc::new(msg)));
            Ok(rebuild_transcript(
                d,
                new_messages,
                transcript_summary_text(d),
                transcript_asset_list(d)?,
                Vec::new(),
                transcript_state(d),
            ))
        }
        _ => Err(VmError::Thrown(VmValue::String(Rc::from(
            "add_tool_result: first argument must be a message list or transcript",
        )))),
    });
}

fn transcript_state(transcript: &BTreeMap<String, VmValue>) -> Option<&str> {
    transcript.get("state").and_then(|value| match value {
        VmValue::String(text) if !text.is_empty() => Some(text.as_ref()),
        _ => None,
    })
}

fn transcript_extra_events(transcript: &BTreeMap<String, VmValue>) -> Vec<VmValue> {
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
    transcript: &BTreeMap<String, VmValue>,
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
    transcript: &BTreeMap<String, VmValue>,
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

fn transcript_inject_reminder_builtin(args: &[VmValue]) -> Result<VmValue, VmError> {
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

    Ok(VmValue::Dict(Rc::new(BTreeMap::from([
        ("transcript".to_string(), next),
        (
            "reminder_id".to_string(),
            VmValue::String(Rc::from(reminder_id)),
        ),
        (
            "deduped_count".to_string(),
            VmValue::Int(deduped_reminder_ids.len() as i64),
        ),
    ]))))
}

fn transcript_clear_reminders_builtin(args: &[VmValue]) -> Result<VmValue, VmError> {
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

    Ok(VmValue::Dict(Rc::new(BTreeMap::from([
        ("transcript".to_string(), next),
        ("removed_count".to_string(), VmValue::Int(removed_count)),
    ]))))
}

fn require_reminder_options<'a>(
    args: &'a [VmValue],
    index: usize,
    context: &str,
) -> Result<&'a BTreeMap<String, VmValue>, VmError> {
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

fn ensure_known_reminder_keys(
    context: &str,
    options: &BTreeMap<String, VmValue>,
    allowed: &[&str],
) -> Result<(), VmError> {
    let unknown = options
        .keys()
        .filter(|key| !allowed.contains(&key.as_str()))
        .map(String::as_str)
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

fn parse_inject_reminder_options(
    options: &BTreeMap<String, VmValue>,
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
    fn matches(&self, reminder: &BTreeMap<String, VmValue>) -> bool {
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
    options: &BTreeMap<String, VmValue>,
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
    options: &BTreeMap<String, VmValue>,
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
    options: &BTreeMap<String, VmValue>,
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

fn reminder_tags(
    options: &BTreeMap<String, VmValue>,
    context: &str,
) -> Result<Vec<String>, VmError> {
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
    options: &BTreeMap<String, VmValue>,
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
    options: &BTreeMap<String, VmValue>,
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
    options: &BTreeMap<String, VmValue>,
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
    options: &BTreeMap<String, VmValue>,
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

fn reminder_payload(event: &VmValue) -> Option<&BTreeMap<String, VmValue>> {
    let event = event.as_dict()?;
    if event.get("kind").map(|value| value.display()).as_deref() != Some(SYSTEM_REMINDER_EVENT_KIND)
    {
        return None;
    }
    event.get("reminder").and_then(VmValue::as_dict)
}

fn reminder_string_field(reminder: &BTreeMap<String, VmValue>, key: &str) -> Option<String> {
    match reminder.get(key) {
        Some(VmValue::String(value)) if !value.is_empty() => Some(value.to_string()),
        _ => None,
    }
}

fn reminder_error(context: &str, message: impl Into<String>) -> VmError {
    VmError::Thrown(VmValue::String(Rc::from(format!(
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
        VmValue::String(Rc::from(value))
    }

    fn dict(entries: Vec<(&str, VmValue)>) -> VmValue {
        VmValue::Dict(Rc::new(
            entries
                .into_iter()
                .map(|(key, value)| (key.to_string(), value))
                .collect(),
        ))
    }

    fn strings(values: &[&str]) -> VmValue {
        VmValue::List(Rc::new(
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
        let first = transcript_inject_reminder_builtin(&[
            base,
            dict(vec![
                ("body", vm_string("first")),
                ("tags", strings(&["context"])),
                ("dedupe_key", vm_string("context")),
            ]),
        ])
        .expect("first inject");
        let second = transcript_inject_reminder_builtin(&[
            result_transcript(&first),
            dict(vec![
                ("body", vm_string("second")),
                ("tags", strings(&["context"])),
                ("dedupe_key", vm_string("context")),
            ]),
        ])
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
        let first = transcript_inject_reminder_builtin(&[
            base,
            dict(vec![
                ("body", vm_string("keep")),
                ("tags", strings(&["keep"])),
            ]),
        ])
        .expect("first inject");
        let second = transcript_inject_reminder_builtin(&[
            result_transcript(&first),
            dict(vec![
                ("body", vm_string("drop")),
                ("tags", strings(&["drop"])),
            ]),
        ])
        .expect("second inject");
        let cleared = transcript_clear_reminders_builtin(&[
            result_transcript(&second),
            dict(vec![("tag", vm_string("drop"))]),
        ])
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
        let err = transcript_inject_reminder_builtin(&[
            base,
            dict(vec![
                ("body", vm_string("hello")),
                ("typo_key", VmValue::Bool(true)),
            ]),
        ])
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
        let err =
            transcript_inject_reminder_builtin(&[base, dict(vec![("body", VmValue::Int(1))])])
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
    fn unknown_reminder_propagate_reports_specific_code() {
        let base = new_transcript_with(None, Vec::new(), None, None);
        let err = transcript_inject_reminder_builtin(&[
            base,
            dict(vec![
                ("body", vm_string("hello")),
                ("propagate", vm_string("workspace")),
            ]),
        ])
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
