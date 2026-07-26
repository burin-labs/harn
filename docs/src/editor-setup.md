# Editor setup

Harn ships a capable language server, `harn-lsp`, that powers diagnostics,
completions, go-to-definition, hover, rename, formatting, and code actions.
The [install script](./getting-started.md) puts `harn-lsp` on your `PATH`
alongside `harn` and `harn-dap`, so any editor with a generic LSP client can
light up Harn support by pointing at that one binary.

`harn-lsp` takes no arguments and speaks the Language Server Protocol over
stdin/stdout. Every editor below launches it the same way — `harn-lsp` with
no flags — and maps the `.harn` file extension to the `harn` language id.

For the full capability matrix (every LSP feature, the DAP debugger, and the
tree-sitter grammar) see [Editor integration](./editor-integration.md).

## VS Code

The richest experience is the bundled extension in `editors/vscode/`, which
wires up the LSP and DAP clients, syntax highlighting, snippets, and the
`Harn: Run Pipeline` / `Harn: Format File` commands automatically.

Once it is published you will be able to install it from the Marketplace or
Open VSX by searching for **Harn Language**. Until then, build and install it
locally:

```bash
cd editors/vscode && npm install && npm run compile && npx @vscode/vsce package
```

Then run **Extensions: Install from VSIX…** from the command palette and pick
the generated `harn-lang-0.1.0.vsix`.

The extension starts `harn-lsp` from your `PATH` by default. Set `harn.lspPath`
in your settings if the binary lives somewhere else.

### Cursor and other VS Code forks

Cursor resolves extensions through [Open VSX](https://open-vsx.org) rather than
the VS Code Marketplace. Once published, search for **Harn Language** there;
until then, package the VSIX with the command above and install it with
`cursor --install-extension harn-lang-0.1.0.vsix`, or use **Extensions: Install
from VSIX…** from the command palette.

### `.harn.prompt` files

Files ending in `.harn.prompt` or `.prompt` open as **Harn Prompt** templates.
The extension gives them:

- Highlighting for directives (`{{ if }}`, `{{ for }}`, `{{ include }}`,
  `{{ section }}`, `{{ raw }}`), `{{# comments #}}`, literals,
  [filters](./prompt-templating.md#filters), and the built-in section names.
- Folding for `{{ if }}` / `{{ for }}` / `{{ section }}` / `{{ raw }}` blocks
  and their matching `{{ end }}` / `{{ endsection }}` / `{{ endraw }}`.
- `{{` → `}}` auto-closing and surrounding, plus `{{#` / `#}}` as the comment
  pair so **Toggle Comment** works inside a template.

The keyword, filter, and section vocabulary in that grammar is generated from
the runtime's template engine, so the editor accepts exactly what
`render(...)` does. Contributors changing the template language should edit
`crates/harn-vm/src/stdlib/template/vocabulary.rs` and run
`make gen-prompt-grammar` rather than hand-editing the grammar; CI fails on
drift.

## Neovim

Neovim's built-in LSP client (0.11+) can start `harn-lsp` directly. First
register the `.harn` filetype, then configure and enable the server. Add this
to your config (for example `~/.config/nvim/init.lua`):

```lua
-- Map the .harn extension to a `harn` filetype.
vim.filetype.add({ extension = { harn = "harn" } })

-- Configure the Harn language server (harn-lsp must be on your PATH).
vim.lsp.config("harn", {
  cmd = { "harn-lsp" },
  filetypes = { "harn" },
  root_markers = { "harn.toml", ".git" },
})
vim.lsp.enable("harn")
```

On older Neovim releases that ship `nvim-lspconfig`, the equivalent is:

```lua
vim.filetype.add({ extension = { harn = "harn" } })

local configs = require("lspconfig.configs")
local lspconfig = require("lspconfig")
if not configs.harn then
  configs.harn = {
    default_config = {
      cmd = { "harn-lsp" },
      filetypes = { "harn" },
      root_dir = lspconfig.util.root_pattern("harn.toml", ".git"),
    },
  }
end
lspconfig.harn.setup({})
```

Open any `.harn` file and run `:LspInfo` (or `:checkhealth lsp`) to confirm the
`harn` client attached. Diagnostics, `gd` (go-to-definition), `K` (hover), and
`vim.lsp.buf.format()` will all work through `harn-lsp`.

## Zed

Zed configures external language servers through its `settings.json`
(**Zed → Settings**, or `~/.config/zed/settings.json`). Map the `.harn`
extension to a language and register `harn-lsp` as its server:

```json
{
  "file_types": {
    "Harn": ["harn"]
  },
  "lsp": {
    "harn-lsp": {
      "binary": {
        "path": "harn-lsp",
        "arguments": []
      }
    }
  },
  "languages": {
    "Harn": {
      "language_servers": ["harn-lsp"]
    }
  }
}
```

Zed resolves a bare `path` against your `PATH`, so the installed `harn-lsp`
binary is picked up with no absolute path. Reopen a `.harn` file and the Harn
language server starts automatically, giving you diagnostics, completions,
hover, and go-to-definition.

## Other editors

Any editor with a generic LSP client — Helix, Emacs (`eglot`/`lsp-mode`),
Sublime Text (LSP), Kate — follows the same recipe: launch `harn-lsp` with no
arguments for `.harn` files. See [Editor integration](./editor-integration.md)
for the full capability list and the tree-sitter grammar that adds syntax
highlighting in tree-sitter-based editors.
