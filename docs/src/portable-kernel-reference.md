# Portable kernel contract

This page specifies Portable Harn Kernel artifact version 1 and its
execute/resume boundary.

## Interfaces

The Rust API is owned by `harn-kernel`. `harn-vm::portable` is the native byte
adapter. `harn-wasm` projects the same operations through generated
`wasm-bindgen` bindings:

```text
compile(source, entry, entry_kind) -> program | diagnostics
start(program, input, grants) -> execution_result
resume(program, snapshot, capability_result, grants) -> execution_result
```

The checked WIT world in `crates/harn-wasm/wit/harn-kernel.wit` expresses the
same value, grant, request, result, and transition types. Core Wasm remains the
browser delivery artifact in v1.

## Program artifact v1

An artifact starts with:

| Field | Encoding | Meaning |
|---|---:|---|
| Magic | 8 bytes | `HARNPK01` |
| Version | big-endian `u16` | `1` |
| Feature flags | big-endian `u16` | Must be zero in v1 |
| Payload length | big-endian `u32` | Exact remaining byte count |
| Payload digest | 32 bytes | BLAKE3 digest of the payload |
| Payload | bounded binary graph | Program image and entry metadata |

The semantic ABI fingerprint inside the payload covers the opcode schema,
portable builtin registry, and typed capability contracts. Opcode bytes and
operand layouts have explicit stable discriminants and a golden fingerprint.
Changing those contracts without an artifact version or compatibility decision
causes a mechanical test failure.

Decoding is an untrusted boundary. It rejects bad magic or version, feature
bits, truncation, trailing bytes, digest corruption, unsupported operations,
invalid instruction boundaries and jumps, cyclic function graphs, inconsistent
builtin identities, excessive type nesting, and all configured byte/count
limits. The decoder reconstructs domain objects; it never deserializes Rust
implementation state.

Default limits include an 8 MiB artifact, 128 levels of type nesting, 16,384
chunks, 16,384 functions, and bounded instruction, constant, string, and
metadata totals. Browser source and JSON inputs are separately limited to 1
MiB; the browser grant document is limited to 64 KiB.

Execution has separate deterministic ceilings:

| Resource | v1 limit |
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

Pure execution uses an empty grant list. A suspendable grant set contains exact
capability method names and a host-retained 32-byte snapshot key. Prefix grants
and wildcards are invalid. Every name and complete argument/result type comes
from the canonical capability registry. The `expected` field is a compact host
display summary; the kernel always validates resumed values against the full
canonical nested type contract before execution continues.

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
as bytes and keep the snapshot key outside that storage. A snapshot is not a
durable cross-version format unless a future contract explicitly says so.

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

## Portable v1 support

| Class | v1 behavior | Examples |
|---|---|---|
| Pure computation | Executes | Records, lists, closures, branching, arithmetic, comparison, slicing, throw/catch, sibling named calls, typed/rest calls, direct and mutual recursion, JSON-shaped input/output |
| Portable host capability | Suspends when exactly granted; otherwise fails | Canonical `harness` methods whose complete parameter and result contracts use `nil`, booleans, integers, floats, strings, bytes, lists, records, unions/options, or literal types |
| Outside v1 | Artifact rejection or exact unsupported diagnostic | Capability contracts containing native objects or unsupported constructors such as channels, streams, closures, schemas, generics, or `Result`; typed default parameters; `for` iteration; concurrency; imports; mutation opcodes; dynamic check opcodes outside the portable call contract; unregistered builtins |

Declared entry and nested-call parameters use the same structural type-contract
matcher as the native VM. That does not make every type-related opcode
portable: `CheckType`, `TryWrapOk`, and `TryUnwrap` remain rejected in v1.
Ordinary `throw`/`catch` control flow is supported; programs whose `Result`
syntax lowers through the rejected wrapping opcodes are not.

Passing type checking is necessary but does not imply portable-v1 support.
The compiler validates the resulting artifact and reports unsupported semantics
before handing bytes to a host.

Capability grants are checked against the canonical registry when the host
constructs a `GrantSet`. A registered method whose full contract cannot cross
the `DataValue` boundary fails with
`unsupported_portable_capability_type`; the kernel never weakens an unsupported
type to `any`. Invocation repeats this check defensively before execution can
suspend.

Untyped default parameters execute portably. A parameter that combines a type
annotation and a default currently fails compilation with
`unsupported_portable_typed_default`; the canonical compiler otherwise lowers
its omitted-value check through the host VM's schema subsystem, which v1 does
not ship into the authority-free kernel.
