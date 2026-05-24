# HARN-STD-101 — public stdlib function missing metadata

## What it means

Every public stdlib function ships with a declarative metadata block above
its `pub fn` declaration so that `harn graph --json`, the LSP hover, generated
docs, and downstream agents can read a single source of truth for the
function's runtime contract.

The required fields are:

| Field            | Purpose                                                                                   |
|------------------|-------------------------------------------------------------------------------------------|
| `@effects`       | Capabilities the function may touch (e.g. `fs.read`, `stdio.write`, `llm.call`). `[]` means pure. |
| `@allocation`    | Allocation profile — typically `stack-only` or `heap`.                                    |
| `@errors`        | Error variants the function may surface. `[]` means infallible.                           |
| `@api_stability` | Stability promise — `stable`, `experimental`, or `deprecated`.                            |
| `@example`       | Minimal usage example (verbatim Harn).                                                    |

The lint warns when a `pub fn` declared inside an embedded stdlib module
(`crates/harn-stdlib/src/stdlib/**/*.harn`) is missing one or more of these
fields from the `/** ... */` HarnDoc block immediately above it.

## How to fix

Add the missing fields to the function's HarnDoc block:

```harn,ignore
/**
 * Render the project README.
 *
 * @effects: [fs.read, fs.write]
 * @allocation: heap
 * @errors: [FileNotFound, PermissionDenied]
 * @api_stability: stable
 * @example: render_readme("README.md")
 */
pub fn render_readme(path: string) -> Result<string, FsError> { ... }
```

The lint considers `@effects: []` and `@errors: []` valid declarations — they
explicitly assert "no effects" and "infallible" respectively, which is what
agents need to read out of the graph.
