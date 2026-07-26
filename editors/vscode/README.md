# Harn Language for VS Code

Editor support for [Harn](https://harnlang.com) — the pipeline-oriented
language for orchestrating AI agents — and for the `.harn.prompt` templates
Harn scripts render.

## What you get

### `.harn` scripts

- Syntax highlighting, snippets, and bracket/indent behaviour.
- Diagnostics, completion, hover, and go-to-definition from the `harn-lsp`
  language server.
- Format on save and `source.fixAll.harn` code actions, both on by default for
  `.harn` files.
- Debugging through the `harn-dap` debug adapter: pick **Debug Current Harn
  File** from the Run and Debug view.
- Commands: **Harn: Run Pipeline**, **Harn: Format File**, **Harn: Apply All
  Autofixes**.
- A `harn` task type for `run`, `check`, `fmt`, `lint`, and `test`, with problem
  matchers that turn compiler and linter output into clickable diagnostics.

### `.harn.prompt` templates

Files ending in `.harn.prompt` or `.prompt` are recognized as **Harn Prompt**
templates and get:

- Highlighting for directives (`{{ if }}`, `{{ for }}`, `{{ include }}`,
  `{{ section }}`, `{{ raw }}`), comments (`{{# ... #}}`), string and numeric
  literals, filters (`{{ name | upper }}`), and the built-in section names.
- Folding for `{{ if }}`/`{{ for }}`/`{{ section }}`/`{{ raw }}` blocks and
  their matching `{{ end }}`/`{{ endsection }}`/`{{ endraw }}`.
- `{{` → `}}` auto-closing and surrounding, and `{{#` / `#}}` as the comment
  pair, so **Toggle Comment** works.

The grammar's keyword, filter, and section vocabulary is generated from the
Harn runtime's template engine (`make gen-prompt-grammar`), so the editor
highlights exactly what the engine accepts — no more and no less.

## Requirements

Highlighting works on its own. The language-server and debugger features need
the Harn toolchain on your `PATH`:

```sh
cargo install harn-cli
```

If the binaries live elsewhere, point the extension at them in settings:

| Setting        | Default    | Purpose                       |
| -------------- | ---------- | ----------------------------- |
| `harn.path`    | `harn`     | The `harn` CLI                |
| `harn.lspPath` | `harn-lsp` | The language server           |
| `harn.dapPath` | `harn-dap` | The debug adapter             |

The extension still loads when the language server is missing; it logs a
warning and falls back to syntax highlighting.

## Installing

### From a registry

Search for **Harn Language** in the VS Code Marketplace. Cursor and other
VS Code forks resolve extensions through [Open VSX](https://open-vsx.org),
where the same extension is published — or install the VSIX directly, below.

### From a VSIX

Every tagged Harn release attaches a packaged `harn-lang.vsix` as a build
artifact of the `Publish VS Code extension` workflow. To install it:

1. Download `harn-lang.vsix`.
2. In VS Code or Cursor, open the Extensions view.
3. From the `...` menu choose **Install from VSIX...** and pick the file.

Or from a terminal:

```sh
code --install-extension harn-lang.vsix     # VS Code
cursor --install-extension harn-lang.vsix   # Cursor
```

### From source

```sh
cd editors/vscode
npm ci
npm run compile
npx @vscode/vsce package --out harn-lang.vsix
```

## Contributing

The extension lives in [`editors/vscode`](https://github.com/burin-labs/harn/tree/main/editors/vscode)
in the Harn repository. `syntaxes/harn-prompt.tmLanguage.json` is generated —
edit `crates/harn-vm/src/stdlib/template/vocabulary.rs` and run
`make gen-prompt-grammar` instead of changing it by hand.

## License

Dual-licensed under MIT or Apache-2.0, at your option. See [LICENSE](LICENSE).
