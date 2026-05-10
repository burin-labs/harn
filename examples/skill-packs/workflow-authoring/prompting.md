# Workflow-authoring prompting (small-model rules)

This guide is consumed both by humans and by the eval harness. It pins down
the response shape so a 4–8B local model (qwen, gemma, llama.cpp) can hit the
validator's enum lists on the first try.

## Output envelope

The model **MUST** return three sections, in this order, each wrapped in a
single XML-style tag with no surrounding prose. The harness strips everything
outside these tags before validating.

```text
<bundle>
{ ... valid WorkflowBundle JSON ... }
</bundle>

<rationale>
- one bullet per design choice
- ≤ 5 bullets, plain prose, no markdown beyond `-`
</rationale>

<verify>
harn workflow validate --bundle out.bundle.json --json
harn workflow preview  --bundle out.bundle.json --json
harn workflow run      --bundle out.bundle.json --json
</verify>
```

The `<bundle>` block is the **single source of truth**. The other two blocks
are advisory; the validator never reads them.

## Hard rules for the JSON

These rules collapse the validator's surface area into a checklist. Keep the
list in front of the model when prompting.

1. `schema_version` is the integer `1`.
2. Every `id` field is non-empty kebab-case ASCII.
3. `workflow._type` is `"workflow_graph"` (literal).
4. `workflow.entry` references a key in `workflow.nodes`.
5. Every key `k` in `workflow.nodes` matches `workflow.nodes[k].id`.
6. Every `workflow.edges[].from` and `.to` references a node key.
7. Every `triggers[].node_id` (when set) references a node key.
8. Trigger kind ∈ `{github, cron, delay, webhook, mcp, manual}`.
   - `github` requires `provider: "github"` and ≥ 1 `events`.
   - `cron` requires `schedule` (5-field cron expression).
   - `delay` requires `delay` as an ISO-8601 duration (`PT10M`, `PT1H`).
   - `webhook` requires `webhook_path` (string starting with `/`).
   - `mcp` requires `mcp_tool` (string).
   - `manual` requires no extra fields.
9. `policy.autonomy_tier` ∈ `{shadow, suggest, act_with_approval, act_auto}`.
10. `policy.retry.max_attempts` ≥ 1; `backoff` is one of
    `{none, fixed, exponential}`.
11. `policy.catchup.mode` ∈ `{none, latest, all}`.
12. `environment.worktree_policy` ∈ `{reuse_current, new_worktree, host_managed}`.
13. Every `prompt_capsules[k].id == k` and points at a real node.
    At most one capsule per node.
14. Every connector that a trigger references via `provider` appears in
    `connectors[]` (otherwise the validator emits a warning).
15. Do not invent fields. Unknown top-level fields stay in `metadata`.

## Anti-patterns (small models fall into these)

- **Inventing trigger kinds** (`pull_request`, `schedule`). Use exactly the
  six kinds in rule 8.
- **String autonomy tiers in PascalCase / TitleCase.** They are snake_case.
- **`max_attempts: 0`.** Always ≥ 1.
- **Mixing capsule keys and ids** (`{ "review": { "id": "review-pr", ... } }`).
  Keys and ids must match.
- **Forgetting `_type: "workflow_graph"`.** It is required for parser
  compatibility with normalized graphs.
- **Embedding markdown fences inside `<bundle>`.** The block must be raw
  JSON only.

## Validation-and-retry loop (the harness implements this)

```text
1. Send the system + user prompt.
2. Extract <bundle>...</bundle>.
3. Parse JSON; if it fails, return parse error to the model and retry once.
4. Run `harn workflow validate --bundle <tmp> --json`.
5. If invalid, return the diagnostics list to the model and retry once.
6. Run `harn workflow preview --bundle <tmp> --json` to confirm graph health.
7. Run `harn workflow run --bundle <tmp> --json` for a deterministic receipt.
```

Cap the retry count at 1 — a model that misses three of the rules above is
not reliably authoring this contract and the harness should fail the case.

## System-prompt skeleton

A working system prompt the eval harness uses by default:

```text
You are authoring a portable Harn workflow bundle. You MUST return
<bundle>, <rationale>, and <verify> blocks exactly as defined in
examples/skill-packs/workflow-authoring/prompting.md.

The <bundle> block contains a single JSON document conforming to
WorkflowBundle (schema_version: 1). Validate against the rules in
that prompting guide. Use the recipes/ directory as concrete examples.
```

## Why XML, not Markdown fences

XML-style tags are robust against the model accidentally closing a fenced
code block early (a common qwen/gemma failure on long JSON). The harness's
extractor is `r"<bundle>([\s\S]+?)</bundle>"` — a markdown fence inside the
block is fine, but no nested `</bundle>` is allowed.
