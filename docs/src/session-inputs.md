# Named session inputs

Named inputs let a host give an agent credentials without putting their values
in model context. Secrets expose names and usage policies. Ordinary variables
also expose their values. Inputs belong to one live session and are excluded
from durable conversation state.

## Private control interface

An authenticated ACP client calls `session/inputs` with a `sessionId` and one
`command`. The command is one of these shapes:

```json
{"kind":"list"}
```

```json
{
  "kind": "put",
  "spec": {
    "name": "SERVICE_KEY",
    "kind": "secret",
    "lifetime": {"kind":"session"},
    "usage": {"kind":"websites","origins":["https://api.example.com"]}
  },
  "value": "private input from the host",
  "replace": false
}
```

```json
{"kind":"remove","name":"SERVICE_KEY"}
```

For a host that owns a conversation across several execution sessions, `sync`
installs an atomic private snapshot: `{kind: "sync", snapshot: {revision,
inputs: [{spec, value}], redactions: []}}`. Produce it with
`SessionInputs::write_private_snapshot`; it includes the host's current grants
and retired secret values needed to filter late output. The runtime refuses
older revisions and treats an identical revision as already applied. Its
response reports `applied` and descriptions. Snapshots are private control
messages and must never enter traces, transcripts, or saved launch documents.

The put request is a private control message, never a tool argument or a
transcript message. Hosts must keep it out of request logging. Successful
responses contain descriptions or removal outcomes, never secret values.
The interface remains available while a prompt waits for input.

Names follow portable environment syntax and are case-sensitive. A name is
unique across both kinds. `variable` permits an empty value; `secret` does not.
`lifetime` defaults to `session`. `until` adds an absolute `unix_ms` expiry.
`saved` records that the host has separately saved the value in its secure
store; Harn still discards its live grant on session close. Harn does not
silently persist a value because its description says `saved`.

`usage` defaults to `session_tools`. `websites` accepts exact HTTPS origins and
disables command use. Paths, URL credentials, queries, fragments, wildcard
hosts, and non-HTTPS origins are refused. Hosts that accept domain names in a
form must turn them into HTTPS origins before submitting them.

The shared implementation bounds a session to 256 inputs, 64 KiB per value,
and 1 MiB of retained private material. Removed and replaced secrets remain
in the bounded output-filtering set until the owner is dropped.

## Harn interface

`harness.secrets.inputs()` lists descriptions. Each contains `name`, `kind`,
`lifetime`, and `usage`. Only ordinary variables have a `value` field.
`harness.secrets.remove_input(name)` removes a live grant and returns whether
one existed. Neither method reads or changes a saved credential.

Bounded `harness.net` requests accept `secret_headers`, a map of HTTP header
names to `{name, prefix?}` bindings. Harn resolves the named value privately,
checks its final destination, and marks the HTTP header sensitive. Such
requests never follow redirects or retry automatically. Downloads and streams
refuse these bindings because their output can bypass the bounded result.

The canonical `harness.tools.run_command` interface accepts `input_env`, a map
of process environment names to session input names. Only declared bindings
reach the child. A binding cannot also appear in `env` or `env_remove`.
Commands with input bindings require foreground execution and pipe capture.
They retain the existing command policy, sandbox, timeout, and cancellation
path. Website-restricted inputs fail before a process starts.

## Host embedding

`harn_execution_policy::session_inputs::SessionInputs` owns validation,
expiry, restriction checks, zeroizing private storage, and descriptions without
a VM dependency. Native hosts can project that same implementation through
their foreign-function interface. `write_private_snapshot` is for private
transport only, never model tool output.

`harn_vm::session_inputs` binds custody to the existing isolated session
runtime, so normal task migration preserves access and independent runtimes do
not share secrets. Forks and restored sessions receive no grants unless their
host explicitly installs them.

## Protection limits

Exact secret values are filtered from normal HTTP results, command output
artifacts, model request bodies, and the existing operational redaction path.
Removed and replaced values remain covered for late output. This complements
pattern-based redaction; it does not depend on a recognizable token prefix.

A program given a secret can transform it, write it elsewhere, or send it to
another server. Output filtering cannot prevent arbitrary transformations.
Use website restrictions when a secret must stay within selected destinations;
Harn then refuses all command bindings for that input. Restrictions cannot
recall a request or revoke a value already held by a running process.
