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
| `harn_version` | Version of the linked `harn-vm` crate |
| `embedded_stdlib_sha256` | Stable digest over every embedded module name and source byte |
| `compatibility.codegen_fingerprint` | Build-time digest of compiler and code-generation inputs |
| `compatibility.bytecode_schema_version` | Bytecode cache format identity |
| `compatibility.linked_program_schema_version` | Linked-program envelope identity |
| `compatibility.linker_algorithm_version` | Linker behavior identity |
| `source_revision` | Full source object ID when measured, otherwise `null` |

`content_sha256` includes every field above except `source_revision`. A build
stamp alone therefore cannot claim different linked content. The optional
revision remains useful provenance, but consumers must keep `null` distinct
from an observed revision.

The embedded standard-library digest has one owner. Bytecode cache invalidation
and this public receipt consume the same VM function, so they cannot drift into
parallel definitions.

`harn version --json` projects this receipt at
`data.runtime_content_fingerprint`. Embedding hosts should project the typed VM
value through their existing version or diagnostic surface instead of hashing
files or reconstructing compatibility fields themselves.
