use std::collections::BTreeMap;

use crate::value::VmValue;

use super::error::TemplateError;

const BUILTIN_SECTIONS: &[&str] = &[
    "task",
    "examples",
    "output_format",
    "tools",
    "thinking_scaffold",
    "chain_of_thought",
    "system_framing",
];

pub(super) struct SectionRender {
    pub(super) text: String,
    pub(super) body_output_start: Option<usize>,
    pub(super) body_source_start: usize,
    pub(super) body_source_end: usize,
}

#[derive(Debug, Default)]
struct SectionProfile {
    prefers_xml_scaffolding: bool,
    prefers_markdown_scaffolding: bool,
    prefers_xml_tools: bool,
    native_tools: bool,
    text_tool_wire_format_supported: bool,
    prefers_role_developer: bool,
    structured_output_mode: String,
    thinking_block_style: String,
}

pub(super) fn is_builtin_section(name: &str) -> bool {
    BUILTIN_SECTIONS.contains(&name)
}

pub(super) fn render_section(
    name: &str,
    body: &str,
    args: &BTreeMap<String, VmValue>,
    llm: Option<&VmValue>,
    line: usize,
    col: usize,
) -> Result<SectionRender, TemplateError> {
    let profile = SectionProfile::from_llm(llm);
    match name {
        "task" => Ok(render_scaffolded("task", "Task", body, &profile)),
        "examples" => Ok(render_scaffolded("examples", "Examples", body, &profile)),
        "system_framing" => Ok(render_system_framing(body, &profile)),
        "output_format" => render_output_format(body, args, &profile, line, col),
        "tools" => render_tools(body, args, &profile, line, col),
        "thinking_scaffold" => Ok(render_thinking_scaffold(body, &profile)),
        "chain_of_thought" => Ok(render_chain_of_thought(body, &profile)),
        _ => Err(TemplateError::new(
            line,
            col,
            format!("unknown template section `{name}`"),
        )),
    }
}

impl SectionProfile {
    fn from_llm(llm: Option<&VmValue>) -> Self {
        let Some(VmValue::Dict(llm)) = llm else {
            return Self {
                structured_output_mode: "none".to_string(),
                thinking_block_style: "none".to_string(),
                ..Self::default()
            };
        };
        let caps = match llm.get("capabilities") {
            Some(VmValue::Dict(caps)) => Some(caps.as_ref()),
            _ => None,
        };
        let structured_output_mode = cap_string(caps, "structured_output_mode")
            .or_else(|| cap_string(caps, "structured_output").map(map_structured_output_mode))
            .unwrap_or_else(|| "none".to_string());
        Self {
            prefers_xml_scaffolding: cap_bool(caps, "prefers_xml_scaffolding"),
            prefers_markdown_scaffolding: cap_bool(caps, "prefers_markdown_scaffolding"),
            prefers_xml_tools: cap_bool(caps, "prefers_xml_tools"),
            native_tools: cap_bool(caps, "native_tools"),
            text_tool_wire_format_supported: cap_bool(caps, "text_tool_wire_format_supported"),
            prefers_role_developer: cap_bool(caps, "prefers_role_developer"),
            structured_output_mode,
            thinking_block_style: cap_string(caps, "thinking_block_style")
                .unwrap_or_else(|| "none".to_string()),
        }
    }

    fn scaffold(&self) -> Scaffold {
        if self.prefers_xml_scaffolding {
            Scaffold::Xml
        } else if self.prefers_markdown_scaffolding {
            Scaffold::Markdown
        } else {
            Scaffold::Plain
        }
    }
}

#[derive(Clone, Copy)]
enum Scaffold {
    Xml,
    Markdown,
    Plain,
}

fn render_scaffolded(
    xml_tag: &str,
    title: &str,
    body: &str,
    profile: &SectionProfile,
) -> SectionRender {
    match profile.scaffold() {
        Scaffold::Xml => wrap_body(body, &format!("<{xml_tag}>\n"), &format!("\n</{xml_tag}>")),
        Scaffold::Markdown => wrap_body(body, &format!("## {title}\n"), ""),
        Scaffold::Plain => wrap_body(body, &format!("{title}:\n"), ""),
    }
}

fn render_system_framing(body: &str, profile: &SectionProfile) -> SectionRender {
    let role = if profile.prefers_role_developer {
        "developer"
    } else {
        "system"
    };
    match profile.scaffold() {
        Scaffold::Xml => wrap_body(body, &format!("<{role}>\n"), &format!("\n</{role}>")),
        Scaffold::Markdown => {
            let title = if profile.prefers_role_developer {
                "Developer Instructions"
            } else {
                "System Instructions"
            };
            wrap_body(body, &format!("## {title}\n"), "")
        }
        Scaffold::Plain => {
            let title = if profile.prefers_role_developer {
                "Developer Instructions"
            } else {
                "System Instructions"
            };
            wrap_body(body, &format!("{title}:\n"), "")
        }
    }
}

fn render_output_format(
    body: &str,
    args: &BTreeMap<String, VmValue>,
    profile: &SectionProfile,
    line: usize,
    col: usize,
) -> Result<SectionRender, TemplateError> {
    if profile.structured_output_mode == "native_json" {
        return Ok(empty_render(body));
    }
    let (body_content, body_source_start, body_source_end) = normalized_body(body);
    let schema = args
        .get("schema")
        .map(pretty_json)
        .transpose()
        .map_err(|error| {
            TemplateError::new(
                line,
                col,
                format!("output_format schema serialization: {error}"),
            )
        })?;
    let mut content = String::new();
    let body_offset = if body_content.is_empty() {
        None
    } else {
        let offset = content.len();
        content.push_str(body_content);
        Some(offset)
    };
    if let Some(schema) = schema {
        if !content.is_empty() {
            content.push_str("\n\n");
        }
        content.push_str("Return output matching this JSON Schema:\n");
        content.push_str(&schema);
    }
    let (prefix, suffix) = match profile.structured_output_mode.as_str() {
        "xml_tagged" => ("<output_format>\n", "\n</output_format>"),
        "delimited" => ("[[ ## output_format ## ]]\n", ""),
        _ => match profile.scaffold() {
            Scaffold::Xml => ("<output_format>\n", "\n</output_format>"),
            Scaffold::Markdown => ("## Output Format\n", ""),
            Scaffold::Plain => ("Output Format:\n", ""),
        },
    };
    Ok(wrap_content_with_mapping(
        &content,
        prefix,
        suffix,
        body_offset,
        body_source_start,
        body_source_end,
    ))
}

fn render_tools(
    body: &str,
    args: &BTreeMap<String, VmValue>,
    profile: &SectionProfile,
    line: usize,
    col: usize,
) -> Result<SectionRender, TemplateError> {
    let tools_json = args
        .get("tools")
        .map(pretty_json)
        .transpose()
        .map_err(|error| TemplateError::new(line, col, format!("tools serialization: {error}")))?;
    let (body_content, body_source_start, body_source_end) = normalized_body(body);
    let mut content = String::new();
    let body_offset = if body_content.is_empty() {
        None
    } else {
        let offset = content.len();
        content.push_str(body_content);
        Some(offset)
    };
    if let Some(tools) = tools_json.as_deref() {
        if !content.is_empty() {
            content.push_str("\n\n");
        }
        content.push_str(tools);
    }
    let render = if profile.prefers_xml_tools {
        wrap_content_with_mapping(
            &content,
            "<tools>\n",
            "\n</tools>",
            body_offset,
            body_source_start,
            body_source_end,
        )
    } else if profile.native_tools {
        wrap_content_with_mapping(
            &content,
            "## Tools\n",
            "",
            body_offset,
            body_source_start,
            body_source_end,
        )
    } else if profile.text_tool_wire_format_supported {
        let mut react = String::from(
            "Tools:\nUse this ReAct-style envelope when calling a tool:\nAction: <tool name>\nAction Input: <json arguments>",
        );
        let mut react_body_output_start = None;
        if !body_content.is_empty() || !content.is_empty() {
            react.push_str("\n\nAvailable tools:\n");
            if !body_content.is_empty() {
                react_body_output_start = Some(react.len());
                react.push_str(body_content);
            }
            if let Some(tools_json) = tools_json.as_deref() {
                if !body_content.is_empty() {
                    react.push_str("\n\n");
                }
                react.push_str(tools_json);
            }
        }
        SectionRender {
            text: react,
            body_output_start: react_body_output_start,
            body_source_start,
            body_source_end,
        }
    } else {
        wrap_content_with_mapping(
            &content,
            "Tools:\n",
            "",
            body_offset,
            body_source_start,
            body_source_end,
        )
    };
    Ok(render)
}

fn render_thinking_scaffold(body: &str, profile: &SectionProfile) -> SectionRender {
    match profile.thinking_block_style.as_str() {
        "thinking_blocks" => render_body_or_default(
            body,
            "Think through the task before answering.",
            "<thinking>\n",
            "\n</thinking>",
        ),
        "reasoning_summary" => render_body_or_default(
            body,
            "Use internal reasoning and provide a concise answer.",
            "## Reasoning\n",
            "",
        ),
        "inline" => render_body_or_default(body, "Reason privately before answering.", "", ""),
        _ => empty_render(body),
    }
}

fn render_chain_of_thought(body: &str, profile: &SectionProfile) -> SectionRender {
    let default = "Reason privately step by step before answering.";
    match profile.scaffold() {
        Scaffold::Xml => render_body_or_default(body, default, "<reasoning>\n", "\n</reasoning>"),
        Scaffold::Markdown => render_body_or_default(body, default, "## Reasoning\n", ""),
        Scaffold::Plain => render_body_or_default(body, default, "", ""),
    }
}

fn wrap_body(body: &str, prefix: &str, suffix: &str) -> SectionRender {
    let (content, source_start, source_end) = normalized_body(body);
    let body_output_start = (!content.is_empty()).then_some(prefix.len());
    let mut text = String::with_capacity(prefix.len() + content.len() + suffix.len());
    text.push_str(prefix);
    text.push_str(content);
    text.push_str(suffix);
    SectionRender {
        text,
        body_output_start,
        body_source_start: source_start,
        body_source_end: source_end,
    }
}

fn render_body_or_default(body: &str, default: &str, prefix: &str, suffix: &str) -> SectionRender {
    let (content, source_start, source_end) = normalized_body(body);
    if content.is_empty() {
        return wrap_content_with_mapping(default, prefix, suffix, None, source_start, source_end);
    }
    wrap_content_with_mapping(content, prefix, suffix, Some(0), source_start, source_end)
}

fn wrap_content_with_mapping(
    content: &str,
    prefix: &str,
    suffix: &str,
    body_offset: Option<usize>,
    body_source_start: usize,
    body_source_end: usize,
) -> SectionRender {
    let body_output_start = body_offset.map(|offset| prefix.len() + offset);
    let mut text = String::with_capacity(prefix.len() + content.len() + suffix.len());
    text.push_str(prefix);
    text.push_str(content);
    text.push_str(suffix);
    SectionRender {
        text,
        body_output_start,
        body_source_start,
        body_source_end,
    }
}

fn empty_render(body: &str) -> SectionRender {
    SectionRender {
        text: String::new(),
        body_output_start: None,
        body_source_start: 0,
        body_source_end: body.len(),
    }
}

fn normalized_body(body: &str) -> (&str, usize, usize) {
    let start = body
        .char_indices()
        .find_map(|(idx, ch)| (!ch.is_whitespace()).then_some(idx))
        .unwrap_or(body.len());
    let end = body
        .char_indices()
        .rev()
        .find_map(|(idx, ch)| (!ch.is_whitespace()).then_some(idx + ch.len_utf8()))
        .unwrap_or(start);
    (&body[start..end], start, end)
}

fn cap_bool(caps: Option<&BTreeMap<String, VmValue>>, key: &str) -> bool {
    matches!(
        caps.and_then(|caps| caps.get(key)),
        Some(VmValue::Bool(true))
    )
}

fn cap_string(caps: Option<&BTreeMap<String, VmValue>>, key: &str) -> Option<String> {
    match caps.and_then(|caps| caps.get(key)) {
        Some(VmValue::String(value)) => Some(value.to_string()),
        _ => None,
    }
}

fn map_structured_output_mode(value: String) -> String {
    match value.as_str() {
        "native" => "native_json".to_string(),
        "tool_use" => "xml_tagged".to_string(),
        "format_kw" => "native_json".to_string(),
        other => other.to_string(),
    }
}

fn pretty_json(value: &VmValue) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(&crate::llm::helpers::vm_value_to_json(value))
}
