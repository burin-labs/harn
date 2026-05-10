# Skill packs

This directory hosts curated skill packs — bundled `SKILL.md` cards, prompts,
recipes, and eval drivers — that an agent can load to author Harn artifacts
correctly on the first try.

Each subdirectory is self-contained and follows the Anthropic / Claude-Code
Agent Skills frontmatter format. A pack typically ships:

- `SKILL.md` — the model-loadable skill card.
- `prompting.md` — output envelope and small-model rules.
- `recipes/<name>/` — validated worked examples.
- `cases/*.case.json` — eval cases with structural assertions.
- `eval.harn` — a Harn driver for live evals against any provider/model.

Packs are paired with Rust regression gates under `crates/harn-cli/tests/`
that load the recipes/cases and assert they still pass the relevant
validation pipeline.

| Pack | Purpose |
|---|---|
| `workflow-authoring/` | Author portable Harn workflow bundles for PR monitoring, repair, and similar engineering automations. |
