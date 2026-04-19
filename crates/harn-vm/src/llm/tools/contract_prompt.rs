use super::collect::{collect_tool_schemas_with_registry, ToolSchema};
use super::params::ToolParamSchema;
use super::type_expr::{ObjectField, TypeExpr};
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
) -> String {
    let mut prompt = String::from("\n\n## Tool Calling Contract\n");
    prompt.push_str(&format!(
        "Active mode: `{mode}`. Follow this runtime-owned contract even if older prompt text suggests another tool syntax.\n\n"
    ));

    if mode == "native" {
        prompt.push_str(NATIVE_CALL_CONTRACT_HELP);
        if require_action {
            prompt.push_str(
                "\nThis turn is action-gated. If tools are available, open your response \
                 with a native tool call, not prose. Do not emit raw source code, diffs, \
                 JSON, or `##DONE##` before the first successful tool action.\n",
            );
        }
        if include_task_ledger_help {
            prompt.push_str(TASK_LEDGER_HELP);
        }
    } else {
        // Front-load format + examples before schemas so weaker models
        // see the calling convention while attention is strongest.
        prompt.push_str(TEXT_RESPONSE_PROTOCOL_HELP);
        if require_action {
            prompt.push_str(
                "\nThis turn is action-gated. If tools are available, open your response \
                 with a tool call (`<tool_call>...</tool_call>`), not prose. Do not emit \
                 raw source code, diffs, JSON, or a <done> block before the first tool call.\n",
            );
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
            prompt.push_str(TASK_LEDGER_HELP);
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

/// Help text for the fenceless TS call syntax. Declared as a constant so tests
/// can assert on its content without duplicating the string.
///
/// The text is written to minimise backtick-counting demands on weaker models:
/// prose references to single-character syntax use quoted descriptions
/// ('backtick', 'double quote') and the ONE code example is embedded in the
/// paragraph without any wrapping fence. Wrapping the example in a Markdown
/// fenced code block caused confusion because models had to balance several
/// levels of backticks at once.
pub(crate) const TEXT_RESPONSE_PROTOCOL_HELP: &str = "
## Response protocol

Every response must be a sequence of these tags, with only whitespace between them:

<tool_call>
name({ key: value })
</tool_call>

<assistant_prose>
Short narration. Optional.
</assistant_prose>

<user_response>
Final user-facing answer. Optional.
</user_response>

<done>##DONE##</done>

Rules the runtime enforces:

- No text, code, diffs, JSON, or reasoning outside these tags. Any stray content is rejected with structured feedback.
- `<tool_call>` wraps exactly one bare call `name({ key: value })`. Do not quote or JSON-encode the call. Use heredoc `<<TAG` ... `TAG` for multiline string fields — raw content, no escaping. Place TAG at the start of the closing line; closing punctuation like `},` may follow on that same line.
- `<assistant_prose>` is optional and must be brief. Never paste source code, file contents, command transcripts, or long plans here — wrap those in the relevant tool call instead.
- `<user_response>` is optional and reserved for the final user-facing answer that hosts should surface. When present, keep it concise and grounded.
- `<done>##DONE##</done>` signals task completion. Emit it only after a successful verifying tool call; the runtime rejects it otherwise.
- Do not prefix calls with labels like `tool_code:`, `python:`, `shell:`, or any language tag, and do not wrap tool calls in Markdown fences.
- Prefer `<tool_call>` over `<assistant_prose>`. If you have nothing concrete to say, omit prose entirely.

Example of a well-formed response:

<assistant_prose>Creating the test file.</assistant_prose>
<user_response>Created the test file.</user_response>
<tool_call>
edit({ action: \"create\", path: \"tests/test_foo.py\", content: <<EOF
def test_foo():
    assert foo() == 42
EOF
})
</tool_call>
";

pub(crate) const NATIVE_CALL_CONTRACT_HELP: &str = "
## Native tool protocol

The provider exposes tool definitions outside this prompt.

- Invoke tools only through the provider's native tool-calling channel.
- The current workflow/system prompt may mention only the tool names available for this stage; do not assume tools from earlier stages remain available.
- Do not write text-mode tool tags, bare `name({ ... })` calls, Markdown code fences, or JSON tool-call envelopes in assistant text.
- Keep assistant prose short and operational. If you emit a final user-facing answer, wrap it in `<user_response>...</user_response>`. When the task is complete and no more tool calls are needed, include `##DONE##` exactly once in assistant text.
";

pub(crate) const TASK_LEDGER_HELP: &str = "
## Task ledger

The runtime may inject a durable `<task_ledger>` of the user's deliverables above this prompt. Only use the `ledger` tool if that `<task_ledger>` block is actually present in the current turn. If no `<task_ledger>` block is present, ignore this section entirely and do not call `ledger(...)`. When a task ledger is present, the `##DONE##` sentinel is rejected while any deliverable is `open` or `blocked`. Use the ledger ids shown in that block; do not invent ids such as `deliverable-N`.

- `ledger({ action: \"add\", text: \"what needs to happen\" })` — declare a new sub-deliverable.
- `ledger({ action: \"mark\", id: \"deliverable-id-from-task-ledger\", status: \"done\" })` — mark a deliverable complete after a real tool call satisfied it.
- `ledger({ action: \"mark\", id: \"deliverable-id-from-task-ledger\", status: \"dropped\", note: \"why\" })` — escape hatch when scope truly changed; the note is required.
- `ledger({ action: \"rationale\", text: \"one-sentence answer to why the user will call this done\" })` — commit to an interpretation of the success criterion.
- `ledger({ action: \"note\", text: \"observation worth remembering across turns\" })` — durable cross-stage memory.

Prefer marking deliverables done only AFTER a concrete tool call demonstrates completion (an edit landed, a run() returned exit 0, a read confirmed an invariant). Don't mark done on prose alone.
";
