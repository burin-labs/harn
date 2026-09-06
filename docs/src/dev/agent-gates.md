# Agent configuration registry reference

`spec/agent-gates/registry.json` owns the configuration census for the runner,
stop decision, and stall handler. Its entry shards record defaults, readers,
canonical headless reachability evidence, and the decision to delete, promote,
or retain each option with an expiry.

The generated tables are:

- [Runner configuration](agent-gates/runner.md)
- [Stop decision configuration](agent-gates/stop-decision.md)
- [Stall handler configuration](agent-gates/stall-handler.md)

## Entry fields

| Field | Contract |
| --- | --- |
| `name` | Unique configuration name, including its owning type or namespace. |
| `kind` | Flag, option, or environment variable. |
| `layer` | Runner, stop decision, or stall handler. |
| `default` | Owning default or an explicit unresolved default requiring review. |
| `readers` | Generated file, line, and function for each structural read. |
| `reachability` | `yes`, `no`, or `unknown` on the canonical headless path. |
| `evidence` | Evidence supporting the reachability claim. A declaration alone does not prove execution. |
| `verdict` | `DELETE`, `PROMOTE-TO-DEFAULT`, or `KEEP-with-expiry`. |
| `reason` | Reason for the verdict. |
| `expiry` | Review date required for retained configuration. |

## Structural coverage

Source scopes use parsed syntax queries for environment reads and native
boundaries. Binding scopes follow typed values, declared function parameters,
aliases, and named normalization results. A parameter or normalization result
can locate the configuration inside a surrounding state object with `path`.
Sibling state fields remain outside that configuration owner.
`binding_scope_files` keeps each owner's binding metadata in a separate typed
file; a missing or malformed file fails the audit. Normalization results can
declare `additional_paths` for distinct views of the same configuration.
Overlapping paths are rejected.
A shared normalizer that preserves an input uses `{function, argument}` with a
zero-based argument index. It carries only an already identified configuration
origin, so unrelated callers of the same helper do not acquire that identity.
Each binding source can add `extra_globs` for consumers outside its primary
`glob`. Every pattern must resolve to source files; overlapping matches are
audited once. Dynamic judge keys have separate literal-domain scopes so a
new key or a missing domain cannot disappear behind a forwarding classification.

Unresolved dynamic keys, opaque calls, and configuration embedded in containers
require explicit ownership evidence. Forwarding and non-behavior classifications
match the observed source expression. Duplicate and stale classifications fail;
a flag read cannot be hidden as a non-behavior fact.

The audit fails on unregistered reads, stale reader locations or projections,
missing source, incomplete parsing, empty source scopes, and stale parameter
mappings. It reports unresolved reachability and entries without observed reads
separately. A zero pending count proves registration within the declared source
scopes. It does not prove that an option fires during a run or improves results.

## Commands and ownership

`make gen-agent-gates` refreshes reader locations and the three tables.
`make check-agent-gates` checks them without rewriting them. CI runs the check.

`std/dev/agent_gates` owns the audit and report contract.
`std/dev/agent_gate_bindings` owns lexical configuration tracking. Repository
scripts supply the registry and capabilities to these shared modules.
