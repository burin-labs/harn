# Migrate to the Portable Harn Kernel

Portable Kernel v1 replaces the former `harn-wasm` source interpreter. The old
adapter parsed and evaluated a subset of Harn independently; it is removed.
This is an intentional breaking cutover.

| Removed surface | Replacement |
|---|---|
| `run`, `execute`, `executePureComponent` | `compile`, then `start` or `resume` |
| `check` | `compile(...).diagnosticsJson()` |
| `tokenize`, `format_code` | Canonical lexer/formatter tooling outside the browser runtime |
| `harn-nativec`, `harn-codegen` | Removed; future acceleration must consume `ProgramArtifact` |

## Browser callers

Replace the old synchronous source-to-output call with three explicit steps:

1. Call `compile(source, entry, entryKind)` and retain `artifactBytes()`.
2. Call `start(artifact, inputJson, grantsJson)` for each fresh execution.
3. If the outcome is suspended, retain `snapshotBytes()` and call `resume`
   after the host produces the matching typed capability result.

Do not cache source-specific JavaScript objects. Cache artifact bytes together
with their digest and invalidate them when the artifact/semantic ABI changes.

Run the adapter in a Web Worker. The interface is synchronous within one
transition and returns to JavaScript at every completion, suspension, or
failure boundary. Running it on the browser main thread can still block
rendering for CPU-heavy pure work.

Parallel browser work uses multiple dedicated workers with independent
execution state. The v1 adapter intentionally does not require Wasm threads,
shared memory, or cross-origin isolation. Native callers may share a decoded
immutable artifact across operating-system threads, but must not share a live
execution state or suspension snapshot between invocations.

## Native embedders

Use `harn_vm::portable::start` and `harn_vm::portable::resume` when consuming
portable artifact bytes. These functions decode with the same untrusted-input
limits and delegate to `harn-kernel`; they do not maintain a native copy of the
portable evaluator.

The full native VM remains the hostful Harn runtime. Portable v1 is not a drop-in
replacement for programs using modules, orchestration, concurrency, generators,
streams, or unsupported builtins.

Typed default parameters compiled to `unsupported_portable_typed_default` in
v1. They are supported from artifact v2 onward: the default is evaluated
through the same shared guard the native runtime uses, so an omitted typed
parameter yields its declared default rather than a diagnostic. Supplied typed
parameters continue to use the shared native/portable structural type matcher.

## Former native-codegen callers

The experimental `harn-codegen` crate and `harn-nativec` command are removed.
They compiled and evaluated a language subset and were not load-bearing. A
future accelerator must consume a validated `ProgramArtifact` and preserve the
kernel's opcode, value, capability, and diagnostic contracts; it must not parse
or evaluate Harn independently.

## Compatibility policy

Portable artifacts reject unknown versions, feature bits, semantic ABI
fingerprints, and unsupported semantics. Recompile from source when
compatibility fails. There is no best-effort decoding and no fallback to the
former interpreter.

## Artifact version 2

Version 2 is the current artifact version. It adds the packaged module closure
and the `NamespaceImportMembers` opcode to the shared bytecode ABI.

**Version 1 artifacts must be recompiled from source.** The kernel fails closed
rather than reading them:

```text
artifact_version: artifact version 1 is not supported; expected 2
```

Recompile a single-file program with `harn portable compile`. When the program
has imports, run `harn portable package` first and compile the resulting
manifest, so the artifact carries its whole module closure and the host never
re-resolves an import.

The adapter interface is unchanged: `compile`, then `start` or `resume`.
