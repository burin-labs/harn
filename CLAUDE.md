# CLAUDE.md

Use [AGENTS.md](AGENTS.md) as the canonical repo guidance.

Before writing or editing `.harn` code, list embedded Harn skills with
`harn skills list --json` and fetch the narrowest guide with
`harn skills get <name> --full`. Start with `harn-language` for syntax and
`harn-orchestration` for trigger, worker, persona, or agent workflow changes.

Claude Code users get the same discovery reminder through
`.claude/skills/harn-scripting/SKILL.md`; the canonical skill content ships in
the local `harn` binary so it matches the version in use.
