use std::collections::HashMap;
use std::fs;
use std::path::Path;

use super::dto::{PortalStorySection, PortalTranscriptMessage, PortalTranscriptStep};
use super::util::preview_text;

pub(super) fn discover_transcript_steps(
    run_dir: &Path,
    relative_path: &str,
) -> Option<Vec<PortalTranscriptStep>> {
    let run_path = run_dir.join(relative_path);
    let stem = run_path.file_stem()?.to_str()?;
    let parent = run_path.parent()?;
    let transcript_path = parent.join(format!("{stem}-llm/llm_transcript.jsonl"));
    if !transcript_path.exists() {
        return None;
    }
    parse_transcript_steps(&transcript_path).ok()
}

fn parse_transcript_steps(path: &Path) -> Result<Vec<PortalTranscriptStep>, String> {
    // Transcripts are an append-only JSONL event stream; reconstruct steps
    // by replaying system_prompt / tool_schemas / message events and
    // crystallizing one PortalTranscriptStep per provider_call_request +
    // response pair.
    let content = fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    let mut steps = Vec::<PortalTranscriptStep>::new();
    let mut by_call = HashMap::<String, usize>::new();
    let mut call_index = 0usize;
    let mut current_system_prompt: Option<String> = None;
    let mut current_schema_names: Vec<String> = Vec::new();
    let mut accumulated_messages: Vec<PortalTranscriptMessage> = Vec::new();
    let mut previous_total: usize = 0;

    for line in content.lines().filter(|line| !line.trim().is_empty()) {
        let raw: serde_json::Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(_) => continue,
        };
        let event_type = raw
            .get("type")
            .and_then(|value| value.as_str())
            .unwrap_or("");

        match event_type {
            "system_prompt" => {
                current_system_prompt = raw
                    .get("content")
                    .and_then(|value| value.as_str())
                    .map(str::to_string);
            }
            "tool_schemas" => {
                current_schema_names = raw
                    .get("schemas")
                    .and_then(|value| value.as_array())
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|item| {
                                item.get("name")
                                    .and_then(|value| value.as_str())
                                    .map(str::to_string)
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
            }
            "message" => {
                accumulated_messages.push(PortalTranscriptMessage {
                    role: raw
                        .get("role")
                        .and_then(|value| value.as_str())
                        .unwrap_or("user")
                        .to_string(),
                    content: raw
                        .get("content")
                        .and_then(|value| value.as_str())
                        .unwrap_or("")
                        .to_string(),
                });
            }
            "provider_call_request" => {
                let call_id = raw
                    .get("call_id")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string();
                if call_id.is_empty() {
                    continue;
                }
                call_index += 1;
                let total_messages = accumulated_messages.len();
                let kept_messages = previous_total.min(total_messages);
                let added_context = accumulated_messages
                    .iter()
                    .skip(kept_messages)
                    .cloned()
                    .collect::<Vec<_>>();
                previous_total = total_messages;
                let step = PortalTranscriptStep {
                    call_id: call_id.clone(),
                    span_id: raw.get("span_id").and_then(|value| value.as_u64()),
                    iteration: raw
                        .get("iteration")
                        .and_then(|value| value.as_u64())
                        .unwrap_or_default() as usize,
                    call_index,
                    model: raw
                        .get("model")
                        .and_then(|value| value.as_str())
                        .unwrap_or("")
                        .to_string(),
                    provider: raw
                        .get("provider")
                        .and_then(|value| value.as_str())
                        .map(str::to_string),
                    kept_messages,
                    added_messages: added_context.len(),
                    total_messages,
                    input_tokens: None,
                    output_tokens: None,
                    system_prompt: current_system_prompt.clone(),
                    added_context,
                    response_text: None,
                    thinking: None,
                    tool_calls: current_schema_names.clone(),
                    summary: "Waiting for model response".to_string(),
                };
                by_call.insert(call_id, steps.len());
                steps.push(step);
            }
            "provider_call_response" => {
                let call_id = raw
                    .get("call_id")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string();
                if let Some(index) = by_call.get(&call_id).copied() {
                    let step = &mut steps[index];
                    step.span_id = step
                        .span_id
                        .or_else(|| raw.get("span_id").and_then(|value| value.as_u64()));
                    step.input_tokens = raw.get("input_tokens").and_then(|value| value.as_i64());
                    step.output_tokens = raw.get("output_tokens").and_then(|value| value.as_i64());
                    step.response_text = raw
                        .get("text")
                        .and_then(|value| value.as_str())
                        .map(str::to_string);
                    step.thinking = raw
                        .get("thinking")
                        .and_then(|value| value.as_str())
                        .map(str::to_string);
                    let response_tool_calls = raw
                        .get("tool_calls")
                        .and_then(|value| value.as_array())
                        .map(|items| {
                            items
                                .iter()
                                .filter_map(|item| {
                                    item.get("name")
                                        .and_then(|value| value.as_str())
                                        .map(str::to_string)
                                })
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    if !response_tool_calls.is_empty() {
                        step.tool_calls = response_tool_calls;
                    }
                    step.summary = summarize_transcript_step(step);
                }
            }
            _ => {}
        }
    }

    Ok(steps)
}

fn summarize_transcript_step(step: &PortalTranscriptStep) -> String {
    if let Some(last_tool) = step.tool_calls.last() {
        return format!(
            "kept {} messages, added {}, then asked for {}",
            step.kept_messages, step.added_messages, last_tool
        );
    }
    if step.response_text.is_some() {
        return format!(
            "kept {} messages, added {}, then replied in text",
            step.kept_messages, step.added_messages
        );
    }
    format!(
        "kept {} messages, added {}",
        step.kept_messages, step.added_messages
    )
}

pub(super) fn build_story(run: &harn_vm::orchestration::RunRecord) -> Vec<PortalStorySection> {
    let mut story = Vec::new();

    if let Some(transcript) = &run.transcript {
        collect_story_sections(transcript, "Run transcript", "run", &mut story);
    }

    for stage in &run.stages {
        if let Some(transcript) = &stage.transcript {
            collect_story_sections(
                transcript,
                &format!("Stage {}", stage.node_id),
                "stage",
                &mut story,
            );
        } else if let Some(text) = &stage.visible_text {
            story.push(PortalStorySection {
                title: format!("Stage {}", stage.node_id),
                scope: "stage".to_string(),
                role: "assistant".to_string(),
                source: "visible_text".to_string(),
                preview: preview_text(text),
                text: text.clone(),
            });
        }
    }

    story
}

fn collect_story_sections(
    value: &serde_json::Value,
    title: &str,
    scope: &str,
    out: &mut Vec<PortalStorySection>,
) {
    if let Some(events) = value.get("events").and_then(|events| events.as_array()) {
        for event in events {
            let role = event
                .get("role")
                .and_then(|value| value.as_str())
                .unwrap_or("assistant");
            let source = event
                .get("kind")
                .and_then(|value| value.as_str())
                .unwrap_or("message");
            let text = extract_event_text(event);
            if text.trim().is_empty() {
                continue;
            }
            out.push(PortalStorySection {
                title: title.to_string(),
                scope: scope.to_string(),
                role: role.to_string(),
                source: source.to_string(),
                preview: preview_text(&text),
                text,
            });
        }
        return;
    }

    if let Some(entries) = value.as_array() {
        for entry in entries {
            let role = entry
                .get("role")
                .and_then(|value| value.as_str())
                .unwrap_or("assistant");
            let text = extract_event_text(entry);
            if text.trim().is_empty() {
                continue;
            }
            out.push(PortalStorySection {
                title: title.to_string(),
                scope: scope.to_string(),
                role: role.to_string(),
                source: "message".to_string(),
                preview: preview_text(&text),
                text,
            });
        }
    }
}

fn extract_event_text(value: &serde_json::Value) -> String {
    if let Some(text) = value.get("text").and_then(|text| text.as_str()) {
        return text.to_string();
    }
    if let Some(content) = value.get("content") {
        if let Some(text) = content.as_str() {
            return text.to_string();
        }
        if let Some(items) = content.as_array() {
            return items
                .iter()
                .filter_map(|item| item.get("text").and_then(|text| text.as_str()))
                .collect::<Vec<_>>()
                .join("\n");
        }
    }
    if let Some(blocks) = value.get("blocks").and_then(|blocks| blocks.as_array()) {
        return blocks
            .iter()
            .filter_map(|item| item.get("text").and_then(|text| text.as_str()))
            .collect::<Vec<_>>()
            .join("\n");
    }
    String::new()
}
