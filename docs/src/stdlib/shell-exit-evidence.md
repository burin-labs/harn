# Shell exit evidence

`std/command.shell_exit_evidence(source, shell)` reports what a zero process
exit establishes about commands visible in a shell line. It never executes or
rewrites the command and never supplies constituent exit codes.

The result has two closed fields:

| Field | Values |
| --- | --- |
| `scope` | `all_commands`, `masked`, `unmeasured` |
| `reason` | `all_commands`, `syntax`, `dialect`, `chain_status`, `pipeline_status`, `compound_command`, `nested_execution`, `shell_state` |

`all_commands` covers a plain POSIX command and plain commands joined with
`&&`. A pipeline also requires an earlier successful `set -o pipefail`. Bash
and Zsh additionally admit a leading `set -o pipefail;` prefix. A literal shell
script file is treated as one external program.

`masked` means a pipeline, fallback, separator or background operation can
hide a constituent failure. `unmeasured` means the grammar, dialect, nested
execution, control flow or shell-state change prevents this proof. Missing
evidence must not be treated as `all_commands`.

The same proof appears in
`command_risk_scan(...).execution_semantics.exit_evidence`, using the request's
resolved execution mode and shell. An argv request preserves its argument
boundaries. It is not reinterpreted as an unquoted shell string.

This contract covers command-line structure. It does not certify a program's
internal checks, imported shell functions, test discovery, collected file
coverage or the correctness of a test assertion. A zero exit and a test count
alone cannot establish that a newly added test file was collected.
