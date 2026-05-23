<!-- markdownlint-disable MD013 MD033 -->

# Concepts

Start here if Harn is new to you, or if you've been writing Harn long enough that the docs you actually want are the ones that explain *why* things are shaped the way they are.

These pages don't teach you syntax and don't list every function. They explain the model that the rest of the docs assume you already have.

<div class="harn-paths">

<div class="harn-path-card">

## [Mental model](./mental-model.md)

The containment diagram for a Harn conversation: how `llm_call`, `agent_loop`, `workflow`, `pipeline`, and `session` fit together.

</div>

<div class="harn-path-card">

## [Glossary](./glossary.md)

Every term Harn uses for a conversational unit, with one-line definitions and pointers to the page that owns each one.

</div>

<div class="harn-path-card">

## [Choosing an abstraction](./abstraction-ladder.md)

When to reach for `llm_call`, `agent_loop`, `agent_turn`, `workflow_execute`, `spawn_agent`, and friends.

</div>

<div class="harn-path-card">

## [Steering seams](./steering-seams.md)

Where you can safely inject a user message into a running agent, and where you can't (yet).

</div>

<div class="harn-path-card">

## [Coming from elsewhere](./sota-comparison.md)

Terminology cross-reference for readers arriving from OpenAI Agents SDK, Anthropic Claude Agent SDK, LangGraph, Inngest, Mastra, ACP, A2A, and MCP.

</div>

</div>

## If you're here to ship code today

- [Getting started](../getting-started.md) — install and run your first program.
- [Tutorials](../tutorial-code-review-agent.md) — guided walkthroughs.
- [Cookbook](../cookbook.md) — task-oriented recipes.
- [Reference](../builtins.md) — every builtin function and option.
