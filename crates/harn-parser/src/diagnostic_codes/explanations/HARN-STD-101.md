# HARN-STD-101 — public stdlib function missing metadata

## What it means

Every public stdlib function ships with a prose summary plus two
machine-meaningful metadata fields above its `pub fn` declaration so that
`harn graph --json`, the LSP hover, generated docs, and downstream agents
can read a single source of truth for the function's runtime contract.

The required fields are:

| Field      | Purpose                                                                                           |
|------------|---------------------------------------------------------------------------------------------------|
| `@effects` | Capabilities the function may touch (e.g. `fs.read`, `stdio.write`, `llm.call`). `[]` means pure. |
| `@errors`  | Error variants the function may surface. `[]` means infallible.                                   |

Two more fields are recognized but optional:

| Field            | Purpose                                                                       |
|------------------|--------------------------------------------------------------------------------|
| `@api_stability` | Stability promise (`experimental`, `internal`, `deprecated`). Absent ⇒ stable. |
| `@example`       | Hand-written usage example. Absent ⇒ tooling derives one from the signature.   |

Only write an `@example` when it shows something the type signature cannot
(non-obvious argument shapes, a multi-step idiom). LSP hover and
`harn graph --json` synthesize a signature-derived example otherwise.

The lint warns when a `pub fn` declared inside an embedded stdlib module
(`crates/harn-stdlib/src/stdlib/**/*.harn`) is missing a required field
from the `/** ... */` HarnDoc block immediately above it.

## How to fix

Add the missing fields to the function's HarnDoc block:

```harn,ignore
/**
 * Render the project README.
 *
 * @effects: [fs.read, fs.write]
 * @errors: [FileNotFound, PermissionDenied]
 */
pub fn render_readme(path: string) -> Result<string, FsError> { ... }
```

The lint considers `@effects: []` and `@errors: []` valid declarations — they
explicitly assert "no effects" and "infallible" respectively, which is what
agents need to read out of the graph.
