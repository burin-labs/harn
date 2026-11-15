# Linked runtime content fingerprint

Embedding hosts can read one VM-owned receipt that identifies the Harn runtime
content linked into their executable:

```rust
let fingerprint = harn_vm::runtime_content_fingerprint();
println!("{}", fingerprint.content_sha256);
```

The function returns `&'static RuntimeContentFingerprint` with these fields:

| Field | Meaning |
| --- | --- |
| `schema` | Receipt schema, currently `harn.runtime_content_fingerprint.v1` |
| `content_sha256` | Composite identity of the linked runtime content |
| `artifact_id` | Linker-owned identity of the containing executable or shared library, or `null` when unmeasured |
| `harn_version` | Version of the linked `harn-vm` crate |
| `embedded_stdlib_sha256` | Stable digest over every embedded module name and source byte |
| `compatibility.codegen_fingerprint` | Digest of compiler inputs and typed builtin signature and authority contracts |
| `compatibility.bytecode_schema_version` | Bytecode cache format identity |
| `compatibility.linked_program_schema_version` | Linked-program envelope identity |
| `compatibility.linker_algorithm_version` | Linker behavior identity |
| `compatibility.build_features` | Closed boolean record of every Cargo feature compiled into `harn-vm` |
| `source_revision` | Full source object ID when measured, otherwise `null` |

`content_sha256` includes `artifact_id`, `harn_version`, `embedded_stdlib_sha256`, and every
compatibility field. It excludes the receipt schema, its own digest, and
`source_revision`. A build stamp alone therefore cannot claim different linked
content. The optional revision remains useful provenance, but consumers must
keep `null` distinct from an observed revision. The library returns `null` for
this field. An embedding executable can clone the receipt and attach its own
measured Harn source revision; it must keep the field `null` when that revision
is unavailable. Build stamps belong to the executable so a new stamp does not
invalidate the runtime library and its unit-test binary.

The embedded standard-library digest has one owner. Bytecode cache invalidation
and this public receipt consume the same VM function, so they cannot drift into
parallel definitions.

The build-feature record distinguishes the distributed full runtime from lean
`default-features = false` embeddings and from optional instrumentation builds.
An owning test compares the record with every feature declared in
`crates/harn-vm/Cargo.toml`, so adding a feature without extending the typed
receipt fails closed.

`harn version --json` projects this receipt at
`data.runtime_content_fingerprint`, with the CLI's compiled source revision.
The CLI ignores caller-supplied environment values for this provenance.
Embedding hosts should project the typed VM
value through their existing version or diagnostic surface instead of hashing
files or reconstructing compatibility fields themselves.

Harn scripts can read the same linked receipt before making a provider call:

```harn
import {
  runtime_content_fingerprint,
} from "std/runtime/content_fingerprint"

fn main(harness: Harness) {
  const identity = runtime_content_fingerprint(harness.runtime)
  harness.stdio.println(identity.content_sha256)
}
```

`harness.runtime.introspection()` and the `current_harn_version` tool include
this value as `runtime_content_fingerprint`. These in-process projections leave
`source_revision` null. The linker identity changes when native component objects
change, including an embedding host when Harn is linked into its executable.
For a shared library, it identifies that library. It is an artifact identity,
not a signature or protection against edits made after linking. Project files
need their own content measurements. Qualification must refuse a missing
`artifact_id`.

Harn MCP servers attach the receipt to each advertised tool's
`_meta["dev.harn/runtime-content"]`. The server supplies this reserved value;
authored tool metadata cannot override it. Agent catalog normalization preserves
it, and `mcp_runtime_content_fingerprint(tool)` from
`std/runtime/content_fingerprint` validates the connected server's receipt.
This identifies the process serving the tool even if its launcher is replaced
on disk. Servers that do not publish the receipt fail validation.
