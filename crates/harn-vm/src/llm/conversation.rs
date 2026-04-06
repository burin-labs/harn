use std::collections::BTreeMap;
use std::rc::Rc;

use crate::value::{VmError, VmValue};
use crate::vm::Vm;

use super::helpers::{
    extract_llm_options, is_transcript_value, new_transcript_with, new_transcript_with_events,
    normalize_transcript_asset, transcript_asset_list, transcript_event, transcript_id,
    transcript_message_list, transcript_summary_text, vm_add_role_message, vm_message_value,
    vm_value_to_json,
};

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
        // Returns a list (messages array) -- can be passed to llm_call via options.messages
        Ok(VmValue::List(Rc::new(Vec::new())))
    });

    vm.register_builtin("transcript", |args, _out| {
        let metadata = args.first().cloned();
        Ok(new_transcript_with(None, Vec::new(), None, metadata))
    });

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
        let prompt = args
            .get(1)
            .and_then(|v| v.as_dict())
            .and_then(|d| d.get("prompt"))
            .map(|v| v.display())
            .unwrap_or_else(|| {
                "Summarize this conversation for a follow-on coding agent. Preserve goals, constraints, decisions, unresolved questions, and concrete next actions. Be concise but complete.".to_string()
            });

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

        opts.messages = vec![serde_json::json!({
            "role": "user",
            "content": format!("{prompt}\n\nConversation:\n{formatted}"),
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
        let archived_count = transcript_message_list(transcript)?.len().saturating_sub(retained.len());
        let mut compacted = match rebuild_transcript(
            transcript,
            retained,
            Some(result.text.clone()),
            transcript_asset_list(transcript)?,
            Vec::new(),
            transcript_state(transcript),
        ) {
            VmValue::Dict(d) => (*d).clone(),
            _ => BTreeMap::new(),
        };
        compacted.insert("archived_messages".to_string(), VmValue::Int(archived_count as i64));
        Ok(VmValue::Dict(Rc::new(compacted)))
    });

    vm.register_builtin("transcript_compact", |args, _out| {
        let transcript = require_transcript(args, "transcript_compact")?;
        let keep_last = args
            .get(1)
            .and_then(|v| v.as_dict())
            .and_then(|d| d.get("keep_last"))
            .and_then(|v| v.as_int())
            .unwrap_or(6)
            .max(0) as usize;
        let messages = transcript_message_list(transcript)?;
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
        let summary = args
            .get(1)
            .and_then(|v| v.as_dict())
            .and_then(|d| d.get("summary"))
            .map(|v| v.display())
            .or_else(|| transcript_summary_text(transcript));
        let mut compacted = match rebuild_transcript(
            transcript,
            retained,
            summary,
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
    mut extra_events: Vec<VmValue>,
    state: Option<&str>,
) -> VmValue {
    let mut preserved = transcript_extra_events(transcript);
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
