# Portable kernel contract

This page specifies Portable Harn Kernel artifact version 2 and its
execute/resume boundary.

## Interfaces

The contract is owned by `harn-kernel`. Harn programs use `std/portable`;
`harn-vm::portable` is its native byte adapter. `harn-wasm` projects the same
operations through generated `wasm-bindgen` bindings:

```text
compile(source, entry, entry_kind) -> program | diagnostics
compilePackage(manifest_json, entry, entry_kind) -> program | diagnostics
start(program, input, grants) -> execution_result
resume(program, snapshot, capability_result, grants) -> execution_result
```

`compile` accepts a self-contained source module. `compilePackage` accepts the
bounded JSON representation below and resolves the complete import closure
before producing bytes. The canonical frontend parses every module and checks
the requested root with imported dependency signatures, matching `harn check`
for an entry file. The manifest is only a transport projection.

```json
{
  "rootSource": "import { increment } from \"math\" ...",
  "rootImports": [{
    "path": "math",
    "target": "module/0",
    "selectedNames": ["increment"],
    "namespaceAlias": null,
    "isPub": false
  }],
  "modules": [{
    "id": "module/0",
    "source": "pub fn increment(amount) { return amount }",
    "imports": [],
    "exports": {"increment": "function"}
  }]
}
```

Targets and module identifiers are validated at the artifact boundary. Missing
exports, duplicate module identifiers, import cycles, and ambiguous bindings
are deterministic diagnostics; the host cannot smuggle an unresolved source
path into execution.

The checked WIT world in `crates/harn-wasm/wit/harn-kernel.wit` expresses the
same value, grant, request, result, and transition types. Core Wasm is the
browser delivery artifact.

Native hosts use the same boundary through the CLI:

```console
harn portable package reducer.harn --output reducer.package.json
harn portable compile reducer.harn --entry reduce --output reducer.hbc
harn portable start reducer.hbc \
  --input event.json \
  --grants grants.json \
  --snapshot-out reducer.snapshot
harn portable resume reducer.hbc \
  --snapshot reducer.snapshot \
  --result capability-result.json \
  --grants grants.json \
  --snapshot-out next.snapshot
```

`package` resolves the filesystem import graph once and writes the data-only
manifest accepted by browser `compilePackage`; it is also the generator behind
the checked-in browser demo manifest. `--check` fails when that projection is
stale.

Each command emits the canonical Harn JSON envelope. `start` and `resume`
return `completed`, `suspended`, or `failed`; a suspended transition writes
the opaque snapshot to the requested path.

## Program artifact v2

An artifact starts with:

| Field | Encoding | Meaning |
|---|---:|---|
| Magic | 8 bytes | `HARNPK01` |
| Version | big-endian `u16` | `2` |
| Feature flags | big-endian `u16` | Must be zero in v2 |
| Payload length | big-endian `u32` | Exact remaining byte count |
| Payload digest | 32 bytes | BLAKE3 digest of the payload |
| Payload | bounded binary graph | Program image and entry metadata |

The semantic ABI fingerprint inside the payload covers the opcode schema,
portable builtin registry, and typed capability contracts. Opcode bytes and
operand layouts have explicit stable discriminants and a golden fingerprint.
Changing those contracts without updating the wire contract causes a
mechanical test failure.

Version 2 adds the `NamespaceImportMembers` opcode to the shared bytecode ABI.
The kernel executes it, and the other import opcodes, against the package
closure recorded in the artifact. Version 1 program artifacts must be
recompiled from source.

Decoding is an untrusted boundary. It rejects bad magic or format number,
feature bits, truncation, trailing bytes, digest corruption, unknown opcodes,
invalid instruction boundaries and jumps, cyclic function graphs, inconsistent
named-call identities, excessive type nesting, and all configured byte/count
limits. The `CallBuiltin` opcode also represents functions,
imports, and captured callable parameters, so decoding verifies its stable
name-derived identity and execution performs the one authoritative lexical
resolution. Known semantics that this kernel cannot execute remain valid
artifact data and produce an exact unsupported diagnostic only when reached.
The decoder reconstructs domain objects; it never deserializes Rust
implementation state.

Default limits include 1 MiB of UTF-8 source, an 8 MiB artifact, 128 levels of
type nesting, 16,384 chunks, 16,384 functions, and bounded instruction,
constant, string, and metadata totals. Browser JSON inputs are separately
limited to 1 MiB; the browser grant document is limited to 64 KiB.

Execution has separate deterministic ceilings:

| Resource | Limit |
|---|---:|
| Instructions (fuel) | 2,000,000 |
| Call frames | 1,024 |
| Lexical scopes | 256 |
| Operand stack values | 16,384 |
| Logical value nodes | 100,000 |
| Value nesting | 128 |
| String and byte payload | 1 MiB |
| Authenticated snapshot | 1 MiB |

## Values

Portable values are null, booleans, signed 64-bit integers, 64-bit floats,
strings, bytes, lists, and string-keyed records. Browser JSON uses tagged
objects for values JSON cannot preserve directly:

```json
{ "$int": "9007199254740993" }
{ "$float": "nan" }
{ "$float": "infinity" }
{ "$bytes": [0, 1, 255] }
```

Ordinary JSON numbers remain ordinary when they are safe and finite. Conversion
enforces byte, node, and depth limits before recursive allocation.

## Grants and capability requests

Pure execution uses `{capabilities: []}`. A suspendable grant record contains
exact capability method names and a host-retained 32-byte snapshot key. Prefix
grants and wildcards are invalid. Every name and complete argument/result type comes
from the canonical capability registry. The `expected` field is a compact host
display summary; the kernel always validates resumed values against the full
canonical nested type contract before execution continues.

Harn calls the optional byte field `snapshot_key`. The browser JSON adapter
calls its 64-character hexadecimal form `snapshotKey`; both project the same
optional field in the WIT `grant-set` record. Bare capability lists are invalid.

A suspension returns:

```text
request: { id, capability, operation, arguments, expected }
snapshot: authenticated bytes
```

The request identifier binds the artifact, operation, arguments, and execution
position. Resume requires the same artifact, the same grant ceiling, the host
snapshot key, and a capability result with that request identifier and expected
shape. Tampering, replay under different grants, a wrong key, or cumulative
fuel exhaustion fails deterministically.

The snapshot is opaque and untrusted outside the kernel. Hosts should store it
as bytes, keep the snapshot key outside that storage, and resume it only with
the artifact and kernel that created it.

## Execution outcomes

| Outcome | Fields | Host action |
|---|---|---|
| `completed` | `value` | Consume the terminal value. |
| `suspended` | `request`, `snapshot` | Perform or deny the typed capability, then resume. |
| `failed` | `diagnostic` | Surface the stable code and message; do not retry implicitly. |

Each start or resume call is isolated and deterministic. A decoded immutable
artifact may be shared across native operating-system threads, while execution
state, grants, fuel, and snapshots are not shared. The canonical browser path
runs each instance in a dedicated Web Worker; the generated bindings do not
enforce worker placement. Multiple workers can reuse the same artifact bytes;
the contract does not require Wasm threads or shared memory.

## Current portable support

| Class | Behavior | Examples |
|---|---|---|
| Pure computation | Executes | Named records and enums, `Result` and `?`, lists and maps, closures, branching, arithmetic, structural equality, slicing, throw/catch, sibling named calls, typed/default/rest calls, direct and mutual recursion, `for` iteration, copy-on-write mutation, pure package imports, JSON-shaped input/output |
| Shared pure builtins | Executes through one native/Wasm implementation | Length and string conversion; trim, replace, prefix checks; hex encode/decode; deterministic path joining; JSON stringify; regex match/replace/captures/split; SHA-256; secret scanning |
| Portable host capability | Suspends when exactly granted; otherwise fails | Canonical `harness` methods whose complete parameter and result contracts use `nil`, booleans, integers, floats, strings, bytes, lists, records, unions/options, or literal types |
| Unavailable | Exact unsupported diagnostic when reached | Capability contracts containing native objects or unsupported constructors such as channels, streams, closures, schemas, or generics; concurrency; hostful operations; callback registration; dynamic check opcodes outside the portable call contract; known but unimplemented builtins |

Declared entry and nested-call parameters use the same structural type-contract
matcher as the native VM. That does not make every type-related opcode
portable: `CheckType` remains rejected. Enum construction and matching,
`TryWrapOk`, and `TryUnwrap` execute in the shared kernel.

Passing type checking is necessary but does not imply portable support.
The compiler validates the resulting artifact structurally. Execution reports
unavailable semantics at their actual trigger, so an unused hostful branch does
not prevent a pure entry path from loading.

Capability grants are checked against the canonical registry when the host
constructs a `GrantSet`. A registered method whose full contract cannot cross
the `DataValue` boundary fails with
`unsupported_portable_capability_type`; the kernel never weakens an unsupported
type to `any`. Invocation repeats this check defensively before execution can
suspend. Typed defaults use the compiler-generated schema contract implemented
by the shared kernel.
