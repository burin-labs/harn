---
name: write-developer-docs
description: Write, revise, or review developer documentation for Harn and Burin. Use for tutorials, how-to guides, reference pages, explanations, README material, migration notes, inline API documentation, and documentation information architecture. Enforces one Diátaxis purpose per page, plain specific prose, canonical terminology, useful links, runnable examples, peer research, and Slopwash-style editing.
---

# Write developer docs

Harn's embedded `harn-docs` skill owns the writing contract used by Harn,
Burin, Codex, and Claude. Read it completely before editing documentation:

```sh
harn skill get harn-docs --full
```

When the current binary predates that skill, read
`crates/harn-skills/src/corpus/harn-docs/SKILL.md` from the Harn checkout.
Follow that version-matched guide for Diátaxis placement, glossary and index
work, peer research, Slopwash editing, examples, links, and checks.
