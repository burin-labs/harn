# Command stage output

`harness.tools.run_command` accepts `capture: {final_stage: true}` to retain
output from the final literal command separately from preceding shell setup.
The option defaults to `false`. Normal command output and execution policy
remain available in the same result.

```harn
fn main(harness: Harness) {
  const result = harness.tools.run_command({
    mode: "shell",
    shell_id: "sh",
    command: "printf 'setup complete\\n' && python -m pytest tests",
    cwd: harness.fs.cwd(),
    capture: {final_stage: true},
  })
  const stage = result?.stage_output
  if stage?.status == "captured" {
    harness.stdio.println(stage.stdout)
  }
}
```

## Result

`std/command` exports the `CommandStageOutput` union. A completed foreground
result, or the terminal `wait_command` result for a background command, includes
`stage_output` when the option was requested. Running background results do not
provide terminal capture evidence.

| Status | Fields | Meaning |
| --- | --- | --- |
| `"captured"` | `argv`, `stdout`, `stderr`, truncation flags, artifact paths and `line_count` | The runner observed the selected stage and finished reading its output |
| `"unmeasured"` | `reason` | The runner cannot attribute a complete stage output |

`argv` contains the selected literal argument list. `stdout_truncated` and
`stderr_truncated` describe the inline strings. `output_path`, `stdout_path`
and `stderr_path` identify the separately retained stage artifacts. Output and
arguments pass through the existing command-output redaction policy.

The regular `stdout`, `stderr` and output artifacts still include preceding
setup output. A consumer that needs stage attribution must require
`stage_output.status == "captured"` and read that stage's fields. Missing or
unmeasured stage output supplies no attribution evidence.

## Supported invocations

Direct argument-vector calls select the whole invocation. On Unix, supported
POSIX shell calls select the final literal external command in a fixed `&&`
chain. Earlier commands retain their original shell scope and short-circuit
behavior, including literal `cd`. Their executions close the private capture
handles. The existing process guardian reserves shell-compatible descriptor
slots and hands output to the existing command readers and terminal waiter.

The runner leaves the original invocation unchanged and reports unmeasured
output for unsupported shell syntax, expansions, redirections, pipelines,
compound control flow, startup state that can change command lookup, unavailable
descriptor slots, file capture transport or an unsupported sandbox transport.
This includes shell declarations such as `export` and shell startup files such
as `BASH_ENV`. A stage that never starts, a cancelled command or a failed output
read also supplies no complete stage capture.

## Interpretation

Captured output identifies a byte stream and a literal argument list. It does
not attest that an executable is a test runner, that it ran tests, or that its
claims are true. A verifier consumer must separately match the argument list
to its declared verifier and apply its existing result parser. Report files
found elsewhere in a workspace are not bound to this stage by this option.

See [Command sequence plans](command-sequence-plan.md) for structural command
comparison without execution.
