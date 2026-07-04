# HARN-LNT-060 — inline options dict bypasses the typed option constructors

## What it means

`agent_loop` and `workflow_execute` accept large option surfaces. Passing an
inline dict literal at the call site skips the typed structural aliases
(`AgentLoopOptions` from `std/agent/options`, `StageSpec` and friends from
`std/workflow/options`), so option typos and wrongly-typed values are only
discovered at runtime — or silently ignored.

The dict still executes exactly as before; this lint is a deprecation-level
nudge toward the single documented path.

## How to fix

- Bind the options to an annotated `let` first:
  `let opts: AgentLoopOptions = {...}` then `agent_loop(task, system, opts)`.
- Or build the options through a typed constructor:
  `agent_preset(kind, {...})` / `agent_options({...})` from
  `std/agent/options`, or `workflow_stage_spec({...})` from
  `std/workflow/options`.
- Suppress the lint only for throwaway probes and fixtures where the inline
  dict is the point.
