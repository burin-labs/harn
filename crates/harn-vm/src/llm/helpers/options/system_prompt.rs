use super::reminders::*;
use super::*;

#[derive(Clone, Copy)]
pub(super) enum SystemPromptPosition {
    Before,
    After,
}

pub(super) fn system_prompt_error(message: impl Into<String>) -> VmError {
    VmError::Thrown(VmValue::String(std::sync::Arc::from(message.into())))
}

pub(super) fn system_prompt_position(
    value: Option<&VmValue>,
    source: &str,
    fallback: SystemPromptPosition,
) -> Result<SystemPromptPosition, VmError> {
    let Some(value) = value else {
        return Ok(fallback);
    };
    match value {
        VmValue::Nil => Ok(fallback),
        VmValue::String(raw) => match raw.as_ref() {
            "before" | "prepend" | "prefix" | "start" => Ok(SystemPromptPosition::Before),
            "after" | "append" | "suffix" | "end" => Ok(SystemPromptPosition::After),
            other => Err(system_prompt_error(format!(
                "{source}.position: expected \"before\" or \"after\", got \"{other}\""
            ))),
        },
        other => Err(system_prompt_error(format!(
            "{source}.position: expected a string, got {}",
            other.type_name()
        ))),
    }
}

pub(super) fn enabled_system_prompt_part(part: &crate::value::DictMap) -> bool {
    !matches!(
        part.get("enabled"),
        Some(VmValue::Bool(false) | VmValue::Nil)
    )
}

pub(super) fn system_prompt_part_content(part: &crate::value::DictMap) -> Option<String> {
    part.get("content")
        .or_else(|| part.get("text"))
        .or_else(|| part.get("prompt"))
        .map(VmValue::display)
}

pub(super) fn render_system_prompt_part(content: String, part: &crate::value::DictMap) -> String {
    let title = part
        .get("label")
        .or_else(|| part.get("title"))
        .or_else(|| part.get("name"))
        .map(VmValue::display)
        .unwrap_or_default();
    let title = title.trim();
    let content = content.trim();
    if title.is_empty() {
        content.to_string()
    } else {
        format!("## {title}\n{content}")
    }
}

/// Expand a host-provided system-prompt option (`system_preamble`,
/// `system_prompt_parts`, …) into [`crate::llm::prompt::PromptFragment`]s,
/// faithfully mirroring the legacy string / list / dict shapes
/// (`{content|text|prompt, position, parts, enabled, label}`). The resulting
/// fragments are reduced by [`crate::llm::prompt::assemble`].
pub(super) fn append_host_fragments(
    out: &mut Vec<crate::llm::prompt::PromptFragment>,
    value: Option<&VmValue>,
    source: &str,
    forced_position: SystemPromptPosition,
) -> Result<(), VmError> {
    use crate::llm::prompt::PromptFragment;
    let Some(value) = value else {
        return Ok(());
    };
    match value {
        VmValue::Nil | VmValue::Bool(false) => Ok(()),
        VmValue::String(text) => {
            out.push(PromptFragment::new(
                format!("host:{source}"),
                format!("host:{source}"),
                fragment_bucket(forced_position),
                text.to_string(),
            ));
            Ok(())
        }
        VmValue::List(items) => {
            for (index, item) in items.iter().enumerate() {
                append_host_fragments(
                    out,
                    Some(item),
                    &format!("{source}[{index}]"),
                    forced_position,
                )?;
            }
            Ok(())
        }
        VmValue::Dict(part) => {
            if !enabled_system_prompt_part(part) {
                return Ok(());
            }
            let position = system_prompt_position(part.get("position"), source, forced_position)?;
            if let Some(parts) = part.get("parts") {
                return append_host_fragments(out, Some(parts), source, position);
            }
            let content = system_prompt_part_content(part).ok_or_else(|| {
                system_prompt_error(format!(
                    "{source}: system prompt part must include `content`, `text`, `prompt`, or `parts`"
                ))
            })?;
            let rendered = render_system_prompt_part(content, part);
            out.push(PromptFragment::new(
                format!("host:{source}"),
                format!("host:{source}"),
                fragment_bucket(position),
                rendered,
            ));
            Ok(())
        }
        other => Err(system_prompt_error(format!(
            "{source}: expected a string, dict, list, nil, or false; got {}",
            other.type_name()
        ))),
    }
}

pub(super) fn fragment_bucket(
    position: SystemPromptPosition,
) -> crate::llm::prompt::FragmentBucket {
    match position {
        SystemPromptPosition::Before => crate::llm::prompt::FragmentBucket::Before,
        SystemPromptPosition::After => crate::llm::prompt::FragmentBucket::After,
    }
}

pub(super) fn system_prompt_fingerprint(system: &str) -> String {
    use sha2::Digest as _;

    let digest = sha2::Sha256::digest(system.as_bytes());
    format!("sha256:{}", hex::encode(digest))
}

pub(crate) fn system_prompt_metadata(system: &str) -> serde_json::Value {
    let fingerprint = system_prompt_fingerprint(system);
    serde_json::json!({
        "content": system,
        "hash": fingerprint,
        "sha256": fingerprint,
        "bytes": system.len(),
    })
}

pub(crate) fn system_prompt_event_metadata(system: &str) -> serde_json::Value {
    let fingerprint = system_prompt_fingerprint(system);
    serde_json::json!({
        "hash": fingerprint,
        "sha256": fingerprint,
        "bytes": system.len(),
    })
}

pub(crate) fn compose_system_prompt(
    system: Option<String>,
    options: Option<&crate::value::DictMap>,
) -> Result<Option<String>, VmError> {
    compose_system_prompt_with_reminders(system, options, &[])
}

pub(super) fn compose_system_prompt_with_reminders(
    system: Option<String>,
    options: Option<&crate::value::DictMap>,
    rendered_reminders: &[RenderedReminder],
) -> Result<Option<String>, VmError> {
    Ok(assemble_system_prompt(system, options, rendered_reminders)?.system)
}

/// Build the system prompt as an ordered list of fragments and reduce them,
/// returning the assembled string together with per-fragment provenance.
///
/// This is the single assembly path: host-provided parts, the primary system
/// text, capability-gated tool guidance, and rendered system reminders all
/// flow through the same [`crate::llm::prompt::assemble`] reducer.
/// [`compose_system_prompt_with_reminders`] is the thin string-only wrapper.
pub(crate) fn assemble_system_prompt(
    system: Option<String>,
    options: Option<&crate::value::DictMap>,
    rendered_reminders: &[RenderedReminder],
) -> Result<crate::llm::prompt::AssembledPrompt, VmError> {
    use crate::llm::prompt::{assemble, FragmentBucket, PromptFragment};

    let mut fragments: Vec<PromptFragment> = Vec::new();
    if let Some(options) = options {
        append_host_fragments(
            &mut fragments,
            options.get("system_preamble"),
            "system_preamble",
            SystemPromptPosition::Before,
        )?;
        append_host_fragments(
            &mut fragments,
            options.get("system_prefix"),
            "system_prefix",
            SystemPromptPosition::Before,
        )?;
        append_host_fragments(
            &mut fragments,
            options.get("system_context"),
            "system_context",
            SystemPromptPosition::Before,
        )?;
        append_host_fragments(
            &mut fragments,
            options.get("system_prompt_parts"),
            "system_prompt_parts",
            SystemPromptPosition::Before,
        )?;
        append_host_fragments(
            &mut fragments,
            options.get("system_appendix"),
            "system_appendix",
            SystemPromptPosition::After,
        )?;
        append_host_fragments(
            &mut fragments,
            options.get("system_suffix"),
            "system_suffix",
            SystemPromptPosition::After,
        )?;
    }

    // The agent loop hands us the primary block pre-decomposed into its
    // constituent parts (system text, MCP advisory, active skills, skill
    // catalog, progress nudge, loop/tool contracts) via `_system_fragments`,
    // so each part is individually auditable instead of opaque inside one
    // joined string. When present it fully supersedes the single-string
    // primary path (the `system` arg / `opts.system` is already represented as
    // one of those fragments).
    let decomposed = append_decomposed_primary_fragments(&mut fragments, options)?;
    if !decomposed {
        let primary_system = system
            .filter(|system| !system.trim().is_empty())
            .or_else(|| {
                options
                    .and_then(|options| options.get("system"))
                    .filter(|value| !matches!(value, VmValue::Nil | VmValue::Bool(false)))
                    .map(VmValue::display)
                    .filter(|system| !system.trim().is_empty())
            });
        if let Some(system) = primary_system {
            fragments.push(PromptFragment::new(
                "primary",
                "primary",
                FragmentBucket::Before,
                system,
            ));
        }
        append_context_profile_fragments(&mut fragments, options);
    }

    // Capability-gated tool guidance: each active tool that declares a
    // `guidance` string contributes an instruction fragment gated on the
    // tool's own presence. Tool and instruction share one source of truth and
    // cannot drift. Dormant until a tool actually carries `guidance`.
    append_tool_guidance_fragments(&mut fragments, options);

    // NB: `RenderedReminder::SystemText` reminders are intentionally NOT folded
    // into the system fragments here. They are appended as a trailing `user`
    // message in `apply_rendered_reminder_messages` so the assembled `system`
    // string stays byte-identical across turns even as the live reminder set
    // changes (token-pressure %, idle nudge, recap TTL), keeping the
    // non-Anthropic prefix cache warm. `RenderedReminder::Message` reminders
    // (Anthropic user blocks / OpenAI developer messages) are likewise handled
    // on the message path with their `cache_control`, never here.
    let _ = rendered_reminders;

    let ctx = assemble_ctx(options);
    Ok(assemble(&fragments, &ctx))
}

/// Names of the tools active for this call, read from the `tools` option
/// (either a list of tool dicts or a `{tools: [...]}` registry).
pub(super) fn tool_names_from_options(
    options: Option<&crate::value::DictMap>,
) -> std::collections::BTreeSet<String> {
    let mut names = std::collections::BTreeSet::new();
    let Some(list) = options.and_then(|options| tool_entry_list(options.get("tools"))) else {
        return names;
    };
    for entry in list.iter() {
        if let Some(name) = entry
            .as_dict()
            .and_then(|dict| dict.get("name"))
            .map(VmValue::display)
            .filter(|name| !name.is_empty())
        {
            names.insert(name);
        }
    }
    names
}

/// Resolve the `tools` option into the flat list of tool dicts, accepting both
/// a bare list and a `{tools: [...]}` registry wrapper.
pub(super) fn tool_entry_list(value: Option<&VmValue>) -> Option<Vec<VmValue>> {
    match value? {
        VmValue::List(items) => Some((**items).clone()),
        VmValue::Dict(dict) => match dict.get("tools") {
            Some(VmValue::List(items)) => Some((**items).clone()),
            _ => None,
        },
        _ => None,
    }
}

/// Append a capability-gated guidance fragment for every active tool that
/// declares a `guidance` (or `system_guidance`) string. The fragment is gated
/// on the tool's own presence so instruction and tool can never drift.
pub(super) fn append_tool_guidance_fragments(
    fragments: &mut Vec<crate::llm::prompt::PromptFragment>,
    options: Option<&crate::value::DictMap>,
) {
    use crate::llm::prompt::{FragmentBucket, PromptFragment};
    let Some(list) = options.and_then(|options| tool_entry_list(options.get("tools"))) else {
        return;
    };
    for entry in list.iter() {
        let Some(dict) = entry.as_dict() else {
            continue;
        };
        let Some(name) = dict
            .get("name")
            .map(VmValue::display)
            .filter(|name| !name.is_empty())
        else {
            continue;
        };
        let guidance = dict
            .get("guidance")
            .or_else(|| dict.get("system_guidance"))
            .map(VmValue::display)
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty());
        let Some(guidance) = guidance else {
            continue;
        };
        fragments.push(
            PromptFragment::new(
                format!("tool:{name}.guidance"),
                format!("tool:{name}"),
                FragmentBucket::Before,
                guidance,
            )
            .requiring_tools(vec![name]),
        );
    }
}

/// Expand the agent loop's `_system_fragments` channel — an ordered list of
/// `{id, source?, body, bucket?, requires_tools?}` dicts — into primary-region
/// [`PromptFragment`]s. This is how `agent_build_turn_system` ships the
/// primary block already decomposed into its parts, so the whole system prompt
/// (not just the host parts and reminders) is auditable through `assemble`.
///
/// Returns `true` if the channel was present (a list), in which case the
/// caller skips the single-string primary path. An empty list still counts as
/// present: the agent computed zero non-empty parts, so there is no primary.
pub(super) fn append_decomposed_primary_fragments(
    fragments: &mut Vec<crate::llm::prompt::PromptFragment>,
    options: Option<&crate::value::DictMap>,
) -> Result<bool, VmError> {
    use crate::llm::prompt::{FragmentBucket, PromptFragment};
    let Some(VmValue::List(items)) = options.and_then(|options| options.get("_system_fragments"))
    else {
        return Ok(false);
    };
    for (index, item) in items.iter().enumerate() {
        let Some(dict) = item.as_dict() else {
            continue;
        };
        let Some(body) = dict.get("body").map(VmValue::display) else {
            continue;
        };
        let id = dict
            .get("id")
            .map(VmValue::display)
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| format!("primary[{index}]"));
        let source = dict
            .get("source")
            .map(VmValue::display)
            .filter(|source| !source.is_empty())
            .unwrap_or_else(|| "primary".to_string());
        let requires_tools = match dict.get("requires_tools") {
            Some(VmValue::List(tools)) => tools.iter().map(VmValue::display).collect(),
            _ => Vec::new(),
        };
        let bucket = match dict
            .get("bucket")
            .map(VmValue::display)
            .map(|bucket| bucket.trim().to_ascii_lowercase())
            .as_deref()
        {
            None | Some("") | Some("before") => FragmentBucket::Before,
            Some("after") => FragmentBucket::After,
            Some(other) => {
                return Err(VmError::Runtime(format!(
                    "_system_fragments[{index}].bucket must be \"before\" or \"after\"; got {other:?}"
                )));
            }
        };
        let requires_caps = match dict.get("requires_caps") {
            Some(VmValue::List(caps)) => caps.iter().map(VmValue::display).collect(),
            _ => Vec::new(),
        };
        fragments.push(
            PromptFragment::new(id, source, bucket, body)
                .requiring_tools(requires_tools)
                .requiring_caps(requires_caps),
        );
    }
    Ok(true)
}

/// Expand a resolved project context profile into prompt fragments. Agent-loop
/// preflight usually forwards these through `_system_fragments`; this path is
/// for direct `llm_call` / `prompt_explain` users that pass `context_profile`.
pub(super) fn append_context_profile_fragments(
    fragments: &mut Vec<crate::llm::prompt::PromptFragment>,
    options: Option<&crate::value::DictMap>,
) {
    use crate::llm::prompt::{FragmentBucket, PromptFragment};
    let Some(profile) = options
        .and_then(|options| {
            options
                .get("context_profile")
                .or_else(|| options.get("project_context_profile"))
        })
        .and_then(VmValue::as_dict)
    else {
        return;
    };
    let Some(VmValue::List(items)) = profile.get("prompt_fragments") else {
        return;
    };
    for (index, item) in items.iter().enumerate() {
        let Some(dict) = item.as_dict() else {
            continue;
        };
        let Some(body) = dict
            .get("body")
            .or_else(|| dict.get("content"))
            .map(VmValue::display)
            .map(|body| body.trim().to_string())
            .filter(|body| !body.is_empty())
        else {
            continue;
        };
        let id = dict
            .get("id")
            .map(VmValue::display)
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| format!("profile[{index}]"));
        let source = dict
            .get("source")
            .map(VmValue::display)
            .filter(|source| !source.is_empty())
            .unwrap_or_else(|| "profile".to_string());
        let requires_tools = match dict.get("requires_tools") {
            Some(VmValue::List(tools)) => tools.iter().map(VmValue::display).collect(),
            _ => Vec::new(),
        };
        let requires_caps = match dict.get("requires_caps") {
            Some(VmValue::List(caps)) => caps.iter().map(VmValue::display).collect(),
            _ => Vec::new(),
        };
        fragments.push(
            PromptFragment::new(id, source, FragmentBucket::Before, body)
                .requiring_tools(requires_tools)
                .requiring_caps(requires_caps),
        );
    }
}

pub(super) fn assemble_ctx(
    options: Option<&crate::value::DictMap>,
) -> crate::llm::prompt::AssembleCtx {
    crate::llm::prompt::AssembleCtx {
        tool_names: tool_names_from_options(options),
        caps: caps_from_options(options),
    }
}

pub(super) fn caps_from_options(
    options: Option<&crate::value::DictMap>,
) -> std::collections::BTreeSet<String> {
    let mut caps = std::collections::BTreeSet::new();
    let Some(options) = options else {
        return caps;
    };
    collect_caps(options.get("caps"), &mut caps);
    collect_caps(options.get("capabilities"), &mut caps);
    if let Some(profile) = options
        .get("context_profile")
        .or_else(|| options.get("project_context_profile"))
        .and_then(VmValue::as_dict)
    {
        collect_caps(profile.get("caps"), &mut caps);
    }
    caps
}

pub(super) fn collect_caps(value: Option<&VmValue>, out: &mut std::collections::BTreeSet<String>) {
    match value {
        Some(VmValue::List(items)) => {
            for item in items.iter() {
                let cap = item.display();
                if !cap.is_empty() {
                    out.insert(cap);
                }
            }
        }
        Some(VmValue::Dict(dict)) => {
            for (key, value) in dict.iter() {
                if !matches!(value, VmValue::Bool(false) | VmValue::Nil) {
                    out.insert(key.clone());
                }
            }
        }
        Some(value) => {
            let cap = value.display();
            if !cap.is_empty() {
                out.insert(cap);
            }
        }
        None => {}
    }
}
