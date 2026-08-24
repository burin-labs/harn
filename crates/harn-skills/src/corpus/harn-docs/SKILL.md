---
name: harn-docs
short: Write task-shaped developer documentation in plain language.
description: Choose one Diátaxis purpose, use canonical Harn terms, prove examples, and remove stock AI prose.
when_to_use: Use when writing or reviewing Harn or Burin tutorials, how-to guides, reference pages, explanations, README text, migration notes, or inline API documentation.
---

# Write Harn developer documentation

Write for the developer who has a task. Pair this skill with
[[harn-language]] for checked Harn examples, [[harn-testing]] for executable
proof, and [[harn-product-quality]] for a complete user path.

## Set the page contract

- Read the nearest `AGENTS.md`, the publishing index, the glossary, and the
  source that owns the behavior.
- Name the reader, their task, and a falsifier for each material claim.
- Give the page one [Diátaxis](https://diataxis.fr/) purpose: tutorial, how-to,
  reference, or explanation.
- Split mixed-purpose pages and link them where the reader needs to switch.
- In Harn, `docs/src/SUMMARY.md` is the published index and
  `docs/src/concepts/glossary.md` owns public terms.
- In Burin, read `site/src/content/README.md` before choosing a help-center
  category.

## Draft from evidence

- Lead with the result or action.
- Use the exact command, type, state, default, limit, error, and path from
  source. Do not invent friendly aliases.
- Explain permissions, side effects, and recovery where they affect a choice.
- Put examples on the canonical product path and run them.
- Link the next local page inline. Link primary external sources for protocols,
  providers, model cards, licenses, and third-party setup.
- For current or comparative claims, inspect the peer page that serves the same
  developer task. Useful agent-framework checks include Flue's
  [quickstart](https://flueframework.com/docs/getting-started/quickstart/),
  [agent guide](https://flueframework.com/docs/guide/building-agents/), and
  [event reference](https://flueframework.com/docs/api/events-reference/).
- State observed facts as facts. Mark inference and remove unsupported claims.

## Edit the prose

Apply the current [Slopwash](https://www.slopwash.com/) rules. Where they are
silent on a question of usage, the
[Google developer documentation style guide](https://developers.google.com/style/)
(CC BY 4.0) is the reference; the
[Microsoft writing style guide](https://learn.microsoft.com/en-us/style-guide/welcome/)
is the second opinion. Neither overrides a rule below.

Harn does not write in
[ASD-STE100 Simplified Technical English](https://asd-ste100.org/). That
standard is built around a roughly 900-word approved dictionary aimed at
aerospace procedures read by non-native speakers, and it is ASD-copyrighted
rather than openly licensed, so it cannot be vendored here. Four of its rules
are worth keeping anyway, because they are checkable rather than a matter of
taste:

- Cap noun clusters at three words. “Provider capability matrix generation
  policy” is five; name the thing instead.
- Keep instructions under about 20 words per sentence, and descriptive prose
  under about 25.
- Use active voice in tutorials and how-to guides. Passive is fine in
  explanation when the actor is genuinely irrelevant.
- Give a word one meaning across the page, and one part of speech.

- Use plain verbs: “is,” “has,” “runs,” and “writes.”
- Use common contractions: “it's,” “you're,” “that's,” “doesn't,” “can't.”
  Contract negations especially — a reader scanning a page skips over “not”
  but cannot misread “don't.” Avoid noun-verb contractions (“Harn's shipping
  a new release”), invented ones, and awkward ones such as “there'd” or
  “it'll.” Keep one form per page rather than mixing “can't” with “cannot.”
- Delete throat-clearing introductions, section summaries, transition filler,
  praise, vague importance, rhetorical questions, and speculative endings.
- Remove staged contrasts, false balance, stock AI vocabulary, decorative bold
  text, emoji, and repeated em dashes.
- Use sentence-case headings. Use a list only when it scans better than prose.
- Keep one exact term instead of cycling through synonyms. Explain unavoidable
  jargon once.
- Prefer a real number, date, state, or failure over an adjective.

Read the page aloud. A tired developer should understand each sentence once.

## Check the result

- Confirm the page's Diátaxis section and index position.
- Follow every link. Run every command and example that can run locally.
- Search for competing terms and copied guidance.
- Run `make check-docs-snippets` for Harn docs, plus the language checks named
  in `AGENTS.md` when syntax or semantics changed.
- Re-read the first sentence of every section as an outline.
- Delete closing sentences that only repeat the section.
- Report the checks that ran and any claim without direct proof.

## Review each page type

For a tutorial:

- Give the reader one complete first success.
- Keep choices out of the main path unless the tutorial teaches that choice.
- Show the expected result after each material action.

For a how-to guide:

- Start from the task and its prerequisites.
- Put the working command before background information.
- Link reference details instead of copying them into the procedure.

For a reference page:

- Mirror the source contract and use a stable, scannable order.
- State defaults, accepted values, return shapes, effects, and errors.
- Generate tables where a registry or schema already owns the rows.

For an explanation:

- Name the design question in the first paragraph.
- Explain ownership, constraints, and tradeoffs with links to the task pages.
- Keep instructions in linked tutorials or how-to guides.

## Keep documentation current

- Change the owning page in the same patch as a public behavior change.
- Search for old names, commands, and defaults across every published surface.
- Prefer generated projections or drift tests when facts repeat across pages.
- Add redirects or migration notes when a renamed page already has readers.
- Remove obsolete pages after their useful content has one clear owner.
