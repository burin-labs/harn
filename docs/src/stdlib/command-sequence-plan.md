# Command sequence plans

`std/command.command_sequence_plan` observes literal shell commands and their
working directories without running them. Use it when comparing a proposed
check against a repository's declared check.

## Signature

```harn
import { command_sequence_plan } from "std/command"

fn main(harness: Harness) {
  const plan = command_sequence_plan("(cd web && npm test) && make check")
  assert_eq(plan.status, "measured")
  assert_eq(plan.steps, [
    {argv: ["npm", "test"], cwd: "web"},
    {argv: ["make", "check"], cwd: ""},
  ])
  const unsupported = command_sequence_plan(
    "export MODE=ci && make check",
  )
  assert_eq(unsupported.status, "unmeasured")
}
```

`command_sequence_plan(source: string, cwd: string = "")` returns
`CommandSequencePlan`:

| Field | Type | Meaning |
| --- | --- | --- |
| `status` | `"measured"` or `"unmeasured"` | Whether the whole sequence has a supported structural interpretation |
| `steps` | `list<CommandSequenceStep>` | Ordered external commands; empty when unmeasured |

Each `CommandSequenceStep` contains `argv: list<string>` and `cwd: string`.
Directories are relative to the repository root. An empty directory means the
root. The optional `cwd` argument sets the starting directory.

## Supported shell forms

The planner uses Harn's existing POSIX shell parser and literal-word decoder.
It recognizes fixed argument lists, `&&`, relative `cd`, and subshells.
Quotes preserve argument boundaries: `'not soak'` is one argument, and quoted
`&&` is text. A directory change applies to subsequent commands in the same
scope. A subshell restores its caller's directory.

An observation is unmeasured for empty input, unsupported syntax, dynamic
arguments, redirects, pipelines, background jobs, other control flow, shell
state changes, or directory escapes. Shell builtins other than relative `cd`
are unmeasured, including `export`, `source`, `exit`, `echo`, and `printf`.
The shared literal decoder also conservatively refuses words containing
expansion or glob characters, even when quoted. Source longer than 65,536 bytes,
nesting deeper than 64 nodes, and sequences beyond 128 steps are unmeasured.

## Interpretation

Require `status == "measured"` and a nonempty `steps` list before comparing
commands. Unsupported or partial input supplies no command evidence.

A measured plan describes literal invocations and directory scopes. It doesn't
prove that an executable exists, that a command ran, or that it passed. It
doesn't grant execution authority. Execution and approval remain with the host;
see [Execution decisions](execution.md).
