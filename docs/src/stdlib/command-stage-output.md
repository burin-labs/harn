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

Both statuses can include `failed_clause: {index: int, total: int, source: string}`.
The indices start at one. A normal nonzero shell exit can identify the last
entered clause after the private observation and guardian completion are confirmed.
`source` contains that clause's original text under the command-output redaction policy.
An earlier clause failure leaves final-stage output unmeasured while retaining
this failure context. Cancellation, failed reads and unconfirmed completion supply
no clause metadata.

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
Literal `export`, `test` and file guards such as `[ -f path ]` also retain their
original shell scope. Expanding their arguments remains unsupported.

Without clause diagnostics, the runner leaves the invocation unchanged and reports unmeasured
output for unsupported shell syntax, expansions, redirections, pipelines,
compound control flow, startup state that can change command lookup, unavailable
descriptor slots, file capture transport or an unsupported sandbox transport.
This includes other shell declarations and shell startup files such as
`BASH_ENV`. A stage that never starts, a cancelled command or a failed output
read also supplies no complete stage capture.

## Clause plan and diagnostics

`std/command.command_output_plan(source: string)` returns `CommandOutputPlan`:

| Field | Type | Meaning |
| --- | --- | --- |
| `status` | `"measured"` or `"unmeasured"` | Whether the source has a supported clause plan |
| `clauses` | `list<string>` | Original clauses in execution order |
| `argv` | `list<string>` | Literal arguments of the final external command |

Unmeasured plans have empty `clauses` and `argv`. Planning never executes a
command or establishes capture availability.

`capture.clause_diagnostics` optionally supplies one plain string per measured
clause. A mismatched count refuses the request before command entry. This option
is used only with `final_stage: true` and defaults to absent.

When startup or transport prevents capture, the runner can add these escaped
strings to the shell source before its one execution. The failed clause's text
appears in normal output. The shell resolver supplies the command-source position;
unsupported source layouts retain the original invocation.

Diagnostic-only execution remains unmeasured and supplies no `failed_clause`.
Callers must not parse diagnostic text into capture or failure authority. When
native clause metadata is available, callers can format their diagnostic text
after completion, outside the captured verifier bytes.

## Interpretation

Captured output identifies a byte stream and a literal argument list. It does
not attest that an executable is a test runner, that it ran tests, or that its
claims are true. A verifier consumer must separately match the argument list
to its declared verifier and apply its existing result parser. Report files
found elsewhere in a workspace are not bound to this stage by this option.

See [Command sequence plans](command-sequence-plan.md) for structural command
comparison without execution.
