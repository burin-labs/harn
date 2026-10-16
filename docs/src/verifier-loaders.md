# Prepared verifier loaders

A prepared verifier executes runner material captured by its host before task
mutation. Its execution receipt identifies that baseline. An ordinary command's
exit code alone doesn't establish verifier identity.

This reference covers `harn_vm::verifier_provenance` and its
[prepared-run authority](./prepared-run.md) integration.

## Host declaration

`RunIntent.verifier_loaders` contains `VerifierLoaderSpec` values.
`HostFacts.admitted_verifier_loaders` must contain the exact same declarations.
Neither a serialized intent nor a model-supplied digest can create native
verifier authority.

| Loader | Rust configuration | Captured material |
| --- | --- | --- |
| `python_source` | `IsolatedPythonSourceVerifier` | Interpreter and entrypoint source |
| `python_module` | `IsolatedPythonModuleVerifier` | Interpreter, module or package source, and declared source dependencies |

Both configurations require `id`, `interpreter`, `workspace`, and `args`.
Interpreter, source, workspace, and search-root paths must already be canonical
and absolute. `id` must be nonempty.

The source loader requires `source`. The module loader requires `module` and
ordered `search_roots`. Its optional `dependencies` lists additional module or
package names to capture from those roots. Dependencies default to an empty
list. Capturing a package includes its Python source submodules.

`invocation_args()` supplies the exact admitted invocation, including `-I -S`.
Use the configured interpreter and workspace with those arguments. A changed
invocation doesn't inherit the verifier's authority.

Preparation reads files without importing them or running startup hooks. The
first measurement for each exact declaration belongs to its `PreparedRun`.
Repeated preparation retains that measurement, including measured absence.
An attached `PreparedSession` retains the same native material across turns.
After process restart, attaching a serialized verifier lease refuses rather
than recapturing changed files.

## Python loader contract

Prepared Python execution is isolated. Ambient `PYTHONPATH`, user-site
directories, startup hooks, bytecode caches, and automatic plugin discovery
don't select its runner. Module search roots are explicit host configuration.
They don't grant filesystem access to child processes.

The source loader registers a separate `__main__` module. The module loader
preserves package initialization, relative imports, `__package__`, `__spec__`,
and the selected entrypoint's `__file__`. Package initialization must reach the
declared `__main__` entrypoint before execution can earn a receipt.

Native extensions, bytecode-only modules, namespace packages, and symlinked
module material aren't supported by this source-loader backend. Modules that
overlap the trusted Python bootstrap are refused. A runner's package data and
test inputs remain ordinary filesystem reads under the process policy.

The interpreter and its installed standard library are host-admitted runtime
material. This contract doesn't attest a malicious interpreter or every library
the runner may load. Python-level audit hooks aren't a substitute for that
boundary; Python documents that malicious code can bypass them.
See [Python auditing](https://docs.python.org/3/library/sys.html#sys.addaudithook).

## Observations and execution receipts

`PreparedVerifier::observe_workspace()` returns a typed observation of the
original filesystem locations:

| Disposition | Meaning |
| --- | --- |
| `unchanged` | Measured locations still match their baseline |
| `changed` | Material differs, including a new earlier search candidate |
| `absent` | Material present in the baseline is now missing, or capture measured absence |
| `unmeasured` | Filesystem access, platform support, or loader behavior prevented measurement |
| `refused` | The declaration or host authority wasn't admitted |

A workspace observation doesn't prove what a later child loaded. Prepared
execution consumes the captured interpreter and source through private native
descriptors. Replacing or deleting the original files can't replace those bytes.

Normal completion returns `verifier_receipt` through `harness.process` execution
and hostlib process completion. It contains `verifier_id`, `baseline_sha256`,
and `executed_material.disposition: "unchanged"`. The baseline digest includes
the exact declaration and captured material identities.

An execution receipt establishes runner identity, not test success. A verifier
pass also requires the observed successful terminal status. Failure to reach
the declared entrypoint refuses completion, including when the process exits
zero. Cancellation and timeout don't publish a verifier receipt.

The workspace may report `changed` while a child executes the original captured
baseline. In that case, only the original runner can earn verification. Tests
can fail, be repaired, and pass without replacing the runner.

## Platform and resource limits

Pinned execution currently requires Linux. Other platforms return an explicit
`unmeasured` disposition. Strict native runtime attestation remains unmeasured.
Ordinary process execution keeps its existing behavior and gains no verifier
qualification merely from executable presence.

Capture ceilings live in the backend's `policy.toml`: 64 MiB per interpreter,
1 MiB per source file, 1,024 source modules, and 16 MiB per encoded bundle.
Exceeding a ceiling refuses preparation.

`CommandPlan.literal_argv` supplies static invocation facts for inspection.
Only `CommandPlan.argv` authorizes shell-to-argv conversion. Startup-dependent
or dynamic shell behavior remains explicitly unmeasured.
