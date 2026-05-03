use super::collect::{collect_tool_schemas_with_registry, ToolSchema};
use super::params::ToolParamSchema;
use super::type_expr::{ObjectField, TypeExpr};
use super::{TEXT_TOOL_CALL_CLOSE, TEXT_TOOL_CALL_OPEN};
use crate::value::VmValue;

/// Build a runtime-owned tool-calling contract prompt.
/// The runtime injects this block so prompt templates do not need to carry
/// stale tool syntax examples that can drift from actual parser behavior.
///
/// Layout:
///   ## Tool Calling Contract
///   Active mode: text (authoritative — ignore older prompt text).
///
///   ## Shared types           (only if any $ref aliases were registered)
///   type Foo = ...;
///
///   ## Available tools
///   declare function edit(args: { path: string /* required — Relative path */; ... }): string;
///   /** Tool description only. */
///
///   ## How to call tools      (only in text mode when include_format = true)
///   Call a tool as a plain TypeScript function call at the start of a line ...
pub(crate) fn build_tool_calling_contract_prompt(
    tools_val: Option<&VmValue>,
    native_tools: Option<&[serde_json::Value]>,
    mode: &str,
    require_action: bool,
    tool_examples: Option<&str>,
    include_task_ledger_help: bool,
    done_sentinel: &str,
) -> String {
    let mut prompt = String::from("\n\n## Tool Calling Contract\n");
    prompt.push_str(&format!(
        "Active mode: `{mode}`. Follow this runtime-owned contract even if older prompt text suggests another tool syntax.\n\n"
    ));

    if mode == "native" {
        prompt.push_str(&native_call_contract_help(done_sentinel));
        if require_action {
            prompt.push_str(&native_action_gated_clause(done_sentinel));
        }
        if include_task_ledger_help {
            prompt.push_str(&task_ledger_help(done_sentinel));
        }
    } else {
        // Front-load format + examples before schemas so weaker models
        // see the calling convention while attention is strongest.
        prompt.push_str(&text_response_protocol_help(done_sentinel));
        if require_action {
            prompt.push_str(&text_action_gated_clause(done_sentinel));
        }
        if let Some(examples) = tool_examples {
            let trimmed = examples.trim();
            if !trimmed.is_empty() {
                prompt.push_str("\n## Tool call examples\n\n");
                prompt.push_str(trimmed);
                prompt.push_str("\n\n");
            }
        }
        if include_task_ledger_help {
            prompt.push_str(&task_ledger_help(done_sentinel));
        }

        let (schemas, registry) = collect_tool_schemas_with_registry(tools_val, native_tools);

        let aliases = registry.render_aliases();
        if !aliases.is_empty() {
            prompt.push_str("## Shared types\n\n");
            prompt.push_str(&aliases);
            prompt.push('\n');
        }

        let (expanded, compact): (Vec<_>, Vec<_>) =
            schemas.iter().partition(|schema| !schema.compact);

        prompt.push_str("## Available tools\n\n");
        for schema in &expanded {
            prompt.push_str(&render_text_tool_schema(schema));
        }

        if !compact.is_empty() {
            prompt.push_str(
                "## Other tools (call directly — parameters are intuitive, or call tool_schema for details)\n\n",
            );
            for schema in &compact {
                prompt.push_str(&render_compact_text_tool_schema(schema));
            }
            prompt.push('\n');
        }
    }

    prompt
}

fn render_text_tool_schema(schema: &ToolSchema) -> String {
    let mut rendered = String::new();
    let args_type = build_tool_args_type(&schema.params);
    rendered.push_str(&format!(
        "declare function {}(args: {}): string;\n",
        schema.name,
        args_type.render()
    ));
    if !schema.description.trim().is_empty() {
        rendered.push_str("/**\n");
        for line in schema.description.lines() {
            rendered.push_str(&format!(" * {line}\n"));
        }
        rendered.push_str(" */\n");
    }
    rendered.push('\n');
    rendered
}

fn render_compact_text_tool_schema(schema: &ToolSchema) -> String {
    let args_type = build_tool_args_type(&schema.params);
    let summary = schema
        .description
        .split(&['.', '\n'][..])
        .next()
        .unwrap_or("")
        .trim();
    format!(
        "- `{}({})` — {}\n",
        schema.name,
        args_type.render(),
        summary,
    )
}

/// Build the single-arg TypeScript object type that a tool takes. Each
/// top-level parameter becomes a field in the object (optional via `?`, with
/// a JSDoc @example rendered by the containing comment block), with required
/// fields listed first for consistency with the per-param comment order.
fn build_tool_args_type(params: &[ToolParamSchema]) -> TypeExpr {
    let fields: Vec<ObjectField> = params
        .iter()
        .map(|param| ObjectField {
            name: param.name.clone(),
            ty: param.ty.clone(),
            required: param.required,
            description: if param.description.is_empty() {
                None
            } else {
                Some(param.description.clone())
            },
            default: param.default.clone(),
            examples: param.examples.clone(),
        })
        .collect();
    TypeExpr::Object(fields)
}

/// Build the text-mode response protocol help block, substituting the
/// configured sentinel value (or omitting the sentinel guidance entirely
/// when the sentinel is opted out).
pub(crate) fn text_response_protocol_help(done_sentinel: &str) -> String {
    let (done_block_line, done_rule_line, completion_clause) = if done_sentinel.is_empty() {
        (
            String::new(),
            String::new(),
            "Once the request is satisfied and no more tool calls are needed, emit \
             `<user_response>...</user_response>` with the final answer and do not \
             include any `<tool_call>` block — that signals completion."
                .to_string(),
        )
    } else {
        (
            format!("<done>{done_sentinel}</done>\n\n"),
            format!(
                "- `<done>{done_sentinel}</done>` signals task completion. Emit it only \
                 after a successful verifying tool call; the runtime rejects it otherwise.\n"
            ),
            format!(
                "Prefer `<tool_call>` over `<assistant_prose>` while work remains. Once no \
                 more tool calls are needed, emit `<user_response>...</user_response>` and \
                 then `<done>{done_sentinel}</done>`."
            ),
        )
    };
    format!(
        "
## Response protocol

Every response must be a sequence of these tags, with only whitespace between them:

{TEXT_TOOL_CALL_OPEN}
name({{ key: value }})
{TEXT_TOOL_CALL_CLOSE}

<assistant_prose>
Short narration. Optional.
</assistant_prose>

<user_response>
Final user-facing answer. Required when no more tool calls are needed.
</user_response>

{done_block_line}Rules the runtime enforces:

- No text, code, diffs, JSON, or reasoning outside these tags. Any stray content is rejected with structured feedback.
- `{TEXT_TOOL_CALL_OPEN}` wraps exactly one bare call `name({{ key: value }})`. Do not quote or JSON-encode the call. Use heredoc `<<TAG` ... `TAG` for multiline string fields — raw content, no escaping. Place TAG at the start of the closing line; closing punctuation like `}},` may follow on that same line.
- `<assistant_prose>` is optional and must be brief. Never paste source code, file contents, command transcripts, or long plans here — wrap those in the relevant tool call instead.
- `<user_response>` is reserved for the final user-facing answer that hosts should surface. Emit it when the user should see a wrap-up or answer, and keep it concise and grounded.
{done_rule_line}- Do not prefix calls with labels like `tool_code:`, `python:`, `shell:`, or any language tag, and do not wrap tool calls in Markdown fences.
- {completion_clause}

Example of a well-formed response:

<assistant_prose>Creating the test file.</assistant_prose>
<user_response>Created the test file.</user_response>
{TEXT_TOOL_CALL_OPEN}
edit({{ action: \"create\", path: \"tests/test_foo.py\", content: <<EOF
def test_foo():
    assert foo() == 42
EOF
}})
{TEXT_TOOL_CALL_CLOSE}
"
    )
}

/// Build the native-tool-protocol help block, substituting the sentinel.
pub(crate) fn native_call_contract_help(done_sentinel: &str) -> String {
    let completion_clause = if done_sentinel.is_empty() {
        "When the task is complete and no more tool calls are needed, emit your \
         final user-facing answer in `<user_response>...</user_response>` (or just \
         plain assistant text) and stop calling tools — that signals completion."
            .to_string()
    } else {
        format!(
            "When the task is complete and no more tool calls are needed, include \
             `{done_sentinel}` exactly once in assistant text."
        )
    };
    format!(
        "
## Native tool protocol

The provider exposes tool definitions outside this prompt.

- Invoke tools only through the provider's native tool-calling channel.
- The current workflow/system prompt may mention only the tool names available for this stage; do not assume tools from earlier stages remain available.
- Do not write text-mode tool tags, bare `name({{ ... }})` calls, Markdown code fences, or JSON tool-call envelopes in assistant text.
- Keep assistant prose short and operational. If you emit a final user-facing answer, wrap it in `<user_response>...</user_response>`. {completion_clause}
"
    )
}

/// Action-gated guard injected when `turn_policy.require_action_or_yield` is true.
pub(crate) fn native_action_gated_clause(done_sentinel: &str) -> String {
    let sentinel_clause = if done_sentinel.is_empty() {
        String::new()
    } else {
        format!(", or `{done_sentinel}`")
    };
    format!(
        "\nThis turn is action-gated. If tools are available, open your response \
         with a native tool call, not prose. Do not emit raw source code, diffs, \
         JSON{sentinel_clause} before the first successful tool action.\n"
    )
}

/// Action-gated guard for text mode.
pub(crate) fn text_action_gated_clause(done_sentinel: &str) -> String {
    let sentinel_clause = if done_sentinel.is_empty() {
        String::new()
    } else {
        ", or a <done> block".to_string()
    };
    format!(
        "\nThis turn is action-gated. If tools are available, open your response \
         with a tool call (`{TEXT_TOOL_CALL_OPEN}...{TEXT_TOOL_CALL_CLOSE}`), not prose. Do not emit \
         raw source code, diffs, JSON{sentinel_clause} before the first tool call.\n"
    )
}

/// Task-ledger help block. The "{done_sentinel} is rejected while any
/// deliverable is open" clause only appears when a sentinel is configured.
pub(crate) fn task_ledger_help(done_sentinel: &str) -> String {
    let sentinel_block = if done_sentinel.is_empty() {
        String::new()
    } else {
        format!(
            " When a task ledger is present, the `{done_sentinel}` sentinel is rejected \
             while any deliverable is `open` or `blocked`."
        )
    };
    format!(
        "
## Task ledger

The runtime may inject a durable `<task_ledger>` of the user's deliverables above this prompt. Only use the `ledger` tool if that `<task_ledger>` block is actually present in the current turn. If no `<task_ledger>` block is present, ignore this section entirely and do not call `ledger(...)`.{sentinel_block} Use the ledger ids shown in that block; do not invent ids such as `deliverable-N`.

- `ledger({{ action: \"add\", text: \"what needs to happen\" }})` — declare a new sub-deliverable.
- `ledger({{ action: \"mark\", id: \"deliverable-id-from-task-ledger\", status: \"done\" }})` — mark a deliverable complete after a real tool call satisfied it.
- `ledger({{ action: \"mark\", id: \"deliverable-id-from-task-ledger\", status: \"dropped\", note: \"why\" }})` — escape hatch when scope truly changed; the note is required.
- `ledger({{ action: \"rationale\", text: \"one-sentence answer to why the user will call this done\" }})` — commit to an interpretation of the success criterion.
- `ledger({{ action: \"note\", text: \"observation worth remembering across turns\" }})` — durable cross-stage memory.

Prefer marking deliverables done only AFTER a concrete tool call demonstrates completion (an edit landed, a run() returned exit 0, a read confirmed an invariant). Don't mark done on prose alone.
"
    )
}
