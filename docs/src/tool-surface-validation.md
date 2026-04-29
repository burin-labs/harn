# Tool Surface Validation

Harn validates agent tool surfaces before a loop or workflow stage spends model
tokens. The validator checks the active tool registry, capability policy,
approval policy, and prompt text for mismatches that would make a tool-calling
loop confusing or unusable.

Use `tool_surface_validate(surface, options?)` for targeted checks:

```harn
let report = tool_surface_validate({
  tools: tools,
  policy: policy,
  approval_policy: approval_policy,
  system: system_prompt,
  tool_search: true,
})
assert(report.valid, "tool surface is coherent")
```

`agent_loop(...)` runs the same validation at startup. Warnings are logged with
stable diagnostic codes; error diagnostics abort the loop before the first model
call. `workflow_validate(...)` and `workflow_policy_report(...)` include the
same diagnostics for workflow and stage tool surfaces.

## Diagnostic Codes

| Code | Severity | Meaning |
| --- | --- | --- |
| `TOOL_SURFACE_MISSING_SCHEMA` | warning | An active tool has no parameter schema. |
| `TOOL_SURFACE_MISSING_ANNOTATIONS` | warning | An active tool has no `ToolAnnotations`. |
| `TOOL_SURFACE_MISSING_SIDE_EFFECT_LEVEL` | warning | An annotated tool left `side_effect_level` as `none`. |
| `TOOL_SURFACE_MISSING_EXECUTOR` | warning | An active tool has no declared executor. |
| `TOOL_SURFACE_MISSING_RESULT_READER` | error | An execute tool can emit artifacts but no active reader can inspect them. |
| `TOOL_SURFACE_UNKNOWN_RESULT_READER` | warning | A declared result reader is not active. |
| `TOOL_SURFACE_UNKNOWN_ARG_CONSTRAINT_TOOL` | warning | A `ToolArgConstraint` references no active tool. |
| `TOOL_SURFACE_UNKNOWN_ARG_CONSTRAINT_KEY` | warning | A constrained argument key is absent from the schema and annotations. |
| `TOOL_SURFACE_APPROVAL_PATTERN_NO_MATCH` | warning | An exact approval-policy tool pattern matches no active tool. |
| `TOOL_SURFACE_UNKNOWN_PROMPT_TOOL` | warning | Prompt text references a tool that is not declared. |
| `TOOL_SURFACE_PROMPT_TOOL_NOT_IN_POLICY` | warning | Prompt text references a declared tool outside the active policy. |
| `TOOL_SURFACE_DEFERRED_TOOL_PROMPT_REFERENCE` | warning | Prompt text references a deferred tool while `tool_search` is inactive. |
| `TOOL_SURFACE_DEPRECATED_ARG_ALIAS` | warning | Prompt text mentions an argument alias instead of its canonical key. |
| `TOOL_SURFACE_SIDE_EFFECT_CEILING_EXCEEDED` | error | A tool requires a higher side-effect level than the policy ceiling. |

## Artifact Readers

Execute tools that may return large output handles should declare that contract
in annotations:

```harn,ignore
annotations: {
  kind: "execute",
  side_effect_level: "process_exec",
  emits_artifacts: true,
  result_readers: ["read_command_output"],
}
```

If a tool always returns complete inline output, set `inline_result: true`
instead of declaring a reader.

## Prompt Suppression

Prompt scans ignore fenced code blocks by default. For historical examples or
non-binding snippets outside fences, use these comment markers:

```text
<!-- harn-tool-surface: ignore-next-line -->
run_command({command: "old example"})

<!-- harn-tool-surface: ignore-start -->
run_command({command: "historical"})
<!-- harn-tool-surface: ignore-end -->
```
