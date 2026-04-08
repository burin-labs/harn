# Editor integration

Harn provides first-class editor support through an LSP server, a DAP
debugger, and a tree-sitter grammar. These cover most modern editors
and IDE workflows.

## VS Code

The `editors/vscode/` directory contains a VS Code extension that bundles
syntax highlighting (via tree-sitter) and automatic LSP/DAP client
configuration.

Install from the extension directory:

```bash
cd editors/vscode && npm install && npm run build
```

Then use **Extensions: Install from VSIX** or symlink into
`~/.vscode/extensions/`.

## Language server (LSP)

Start the LSP server with:

```bash
cargo run --bin harn-lsp
```

Or use the compiled binary directly (`harn-lsp`). The server communicates
over stdin/stdout using the Language Server Protocol.

### Supported capabilities

| Feature | Description |
|---------|-------------|
| **Diagnostics** | Real-time parse errors, type errors, and warnings |
| **Completions** | Scope-aware: pipelines, functions, variables, parameters, enums, structs, interfaces. Dot-completions for methods. Builtins and keywords. |
| **Go-to-definition** | Jump to the declaration of pipelines, functions, and variables |
| **Find references** | Locate all usages of a symbol across the document |
| **Hover** | Shows type information and documentation for builtins |
| **Signature help** | Parameter hints while typing function arguments |
| **Document symbols** | Outline view of pipelines, functions, structs, enums |
| **Workspace symbols** | Cross-file search for pipelines and functions |
| **Semantic tokens** | Fine-grained syntax highlighting for keywords, types, functions, parameters, enums, and more |
| **Code actions** | Quick fixes for lint warnings (var→let, boolean simplification, unused import removal, string interpolation) and type errors |
| **Rename** | Rename symbols across the document |
| **Document formatting** | Delegates to `harn-fmt` for format-on-save support |

### Configuration

Most editors auto-detect the LSP binary. For manual configuration, point
your editor's LSP client at the `harn-lsp` binary with no arguments. The
server uses `TextDocumentSyncKind::FULL` (re-parses the full document on
each change).

## Debug adapter (DAP)

Start the debugger with:

```bash
cargo run --bin harn-dap
```

The DAP server communicates over stdin/stdout using the Debug Adapter
Protocol. It supports:

- Breakpoints (line-based)
- Step in / step over / step out
- Variable inspection in scopes
- Stack frame navigation
- Continue / pause execution

### VS Code launch configuration

```json
{
  "type": "harn",
  "request": "launch",
  "name": "Debug Harn",
  "program": "${file}"
}
```

## Tree-sitter grammar

The `tree-sitter-harn/` directory contains a tree-sitter grammar for Harn.
This powers syntax highlighting in editors that support tree-sitter
(Neovim, Helix, Zed, etc.).

Build the grammar:

```bash
cd tree-sitter-harn && npx tree-sitter generate
```

Highlight queries are in `tree-sitter-harn/queries/highlights.scm`.

## Formatter

Format Harn files from the command line or integrate with editor
format-on-save:

```bash
harn fmt file.harn          # format in place
harn fmt --check file.harn  # check without modifying
```

## Linter

Run the linter for static analysis:

```bash
harn lint file.harn
harn lint --fix file.harn   # automatically apply safe fixes
```

The linter checks for: shadow variables, unused variables, unreachable code,
missing harndoc comments, and other code quality issues. With `--fix`, the
linter automatically rewrites fixable issues (e.g., `var` → `let`,
boolean comparison simplification, unused import removal).
