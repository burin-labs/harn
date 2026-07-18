## Evaluation order

### Program entry

1. All top-level nodes are scanned. Pipeline declarations are registered by name.
   Import declarations are processed (loaded and evaluated).
2. The entry pipeline is selected: the pipeline named `"default"` if it exists,
   otherwise the first pipeline in the file.
3. The entry pipeline's body is executed.

If no pipeline is found in the file, all top-level statements are compiled
and executed directly as an implicit entry point (script mode). This allows
simple scripts to work without wrapping code in a pipeline block.

### Pipeline parameters

If the pipeline parameter list includes `task`, it is bound to `context.task`.
If it includes `project`, it is bound to `context.projectRoot`.
A `context` dict is always injected with keys `task`, `project_root`, and `task_type`.

Pipeline parameters accept the same optional `name: TypeExpr` annotations as
function parameters. The type checker uses declared types in the pipeline body
and at local or imported call sites:

```harn
pub pipeline deploy(config: DeployConfig, dry_run: bool) -> bool {
  return !dry_run
}
```

Legacy untyped parameters remain valid syntax. Packages can require complete
annotations on public functions and pipelines with
`[lint] require_public_api_types = true`. Pipeline default values and rest
parameters are rejected because pipeline invocation does not define those
runtime semantics.

### Pipeline return type

Pipelines may declare a return type with the same `-> TypeExpr` syntax
as functions:

```harn
pipeline ghost_text(task) -> {text: string, code: int} {
  return {text: "hello", code: 0}
}
```

The type checker verifies every `return <expr>` statement against the
declared type. Mismatches are reported as `return type doesn't match`
errors.

A declared return type is the typed contract that a host or bridge
(ACP, A2A) can rely on when consuming the pipeline's output.

Public pipelines (`pub pipeline`) without an explicit return type emit the
`pipeline-return-type` lint warning by default. When
`require_public_api_types` is enabled, `missing-public-api-type` owns both
parameter and return completeness without duplicate diagnostics.

### Pipeline inheritance

`pipeline child(x) extends parent { ... }`:

- If the child body contains `override` declarations, the resolved body is the parent's
  body plus any non-override statements from the child.
  Override declarations are available for lookup by name.
- If the child body contains no `override` declarations, the child body entirely replaces the parent body.

### Statement execution

Statements execute sequentially. The last expression value in a block is the block's result,
though this is mostly relevant for closures and parallel bodies.

### Import resolution

`import "path"` resolves in this order:

1. If path starts with `std/`, loads embedded stdlib module (e.g. `std/text`)
2. Relative to current file's directory; auto-adds `.harn` extension
3. `<current-generation>/packages/<path>` in the leased package snapshot
   published by the nearest ancestor package root (the search walks upward
   and stops at a `.git` boundary).
4. Package manifest `[exports]` mappings under that snapshot's
   `packages/<package>/harn.toml`
5. Package directories with `lib.harn` entry point

Package manifests can publish stable module entry points without forcing
consumers to import the on-disk file layout directly:

```toml
[exports]
capabilities = "runtime/capabilities.harn"
providers = "runtime/providers.harn"
```

With the example above, `import "acme/capabilities"` resolves to the
declared file inside the installed `acme` package.

#### Export visibility

A module's **export surface** — the set of names other modules can import,
whether by wildcard (`import "m"`) or selectively (`import { x } from "m"`) —
is exactly the declarations it marks `pub`, plus any `pub import` re-exports.
`pub` may prefix any top-level declaration: `fn`, `tool`, `skill`, `eval_pack`,
`struct`, `enum`, `type`, `pipeline`, and — for shared configuration and prompt
constants — top-level `const` and `let` value bindings. Non-`pub` declarations
are private to the module: usable by the module's own functions, but not
importable by name or by wildcard. A module that marks nothing `pub` exports
nothing.

A `pub const` / `pub let` is exported **by value**: the binding's value is
computed once when the module is instantiated, then bound into each importer.
Later mutation of a `pub let` in its defining module is not observed by
importers (cross-module values are by-value, like every other imported value).

This is the explicit-visibility model of Rust, Go, and TypeScript. Harn does
**not** have a "public-by-default until the first `pub`" rule: such a rule
makes adding the first `pub` a silent breaking change, because it would flip
every *other* function from importable to private. Requiring `pub` up front
keeps a module's export surface stable as it grows.

The same rule applies to both import forms — a selective import cannot reach a
private function that a wildcard import would not see. Importing a non-`pub`
name is an error (`HARN-IMP-002`) at `harn check` time and at load time; the
message points at the import and suggests marking the symbol `pub`.

**Testing private functions.** A non-`pub` function is visible to any
`pipeline` or `fn` declared in the **same file**, so co-locate unit tests with
the code under test (the Rust/Go white-box pattern) rather than importing the
private name into a separate test module.

Selective imports: `import { name1, name2 } from "module"` imports only
the specified functions, each of which must be part of the module's export
surface (see above).

Scoped selective imports are shorthand for slash-delimited module paths:
`import std::personas::prelude::{verify_then_act}` is equivalent to
`import { verify_then_act } from "std/personas/prelude"`.

Public re-exports: prefixing any `import` with `pub` re-exports the
imported symbols as part of the importing module's public surface, so
downstream importers see them as if they were declared there directly:

- `pub import "module"` — re-export every name the target module
  exports. Equivalent to wildcard re-export.
- `pub import { name } from "module"` — re-export only the listed
  names. Other names from the source module remain private to the
  importing module.

Re-exports compose: a facade module that `pub import`s from another
facade transitively forwards every reachable name. Two re-exports of
the same name from different sources — or a re-export that shadows a
local `pub` declaration — are reported by `harn check` as a re-export
conflict naming every contributing module.

Imported pipelines are registered for later invocation.
Non-pipeline top-level statements (fn declarations, let bindings) are executed immediately.

Import cycles: modules may import each other (directly or transitively).
A plain `import "m"` or selective `import { name } from "m"` that resolves
to a module still mid-load is **bound late** — once every module in the
cycle finishes loading, the name resolves for both bare references and
calls, regardless of the order the modules happened to load in. A
`pub import` re-export across a cycle is **not** supported: re-exporting
must publish the name into the importing module's public surface
immediately, but that surface does not exist yet while the cycle is
loading, so it is a load error that names the cycle. Use a plain `import`
inside the cycle and re-export from a module outside it.

### Static cross-module resolution

`harn check`, `harn run`, `harn bench`, and the LSP build a **module graph**
from the entry file that transitively loads every `import`-reachable
`.harn` module. The graph drives:

- **Typechecker**: when every import in a file resolves, call targets
  that are not builtins, not local declarations, not struct constructors,
  not callable variables, and not introduced by an import produce a
  `call target ... is not defined or imported` **error** (not a lint
  warning). This catches typos and stale imports before the VM loads.
- **Linter**: wildcard imports are resolved via the same graph; the
  `undefined-function` rule can now check against the actual exported
  name set of imported modules rather than silently disabling itself.
- **LSP go-to-definition**: cross-file navigation walks the graph's
  `definition_of` lookup, so any reachable symbol (through any number of
  transitive imports) can be jumped to.

Resolution conservatively **degrades to the pre-v0.7.12 behavior** when
any import in the file is unresolved (missing file, parse error,
non-existent package directory), so a single broken import does not
avalanche into a sea of false-positive undefined-name errors. The
unresolved import itself still surfaces via the runtime loader.
