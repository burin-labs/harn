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

The bundled extension in `editors/vscode/` adds syntax highlighting, snippets,
the language server, the debugger, and Harn commands. Install it from source
until a registry release is available.

### Install the extension from source

Install the [Harn toolchain](./getting-started.md#install-harn) first. Then clone
the Harn repository, build a VSIX package, and install it:

```bash
git clone https://github.com/burin-labs/harn.git
cd harn/editors/vscode
npm ci
npm run compile
npx @vscode/vsce package --out harn-lang.vsix
code --install-extension harn-lang.vsix
```

You can instead run **Extensions: Install from VSIX...** from the command
palette and select `harn-lang.vsix`.

The extension starts `harn-lsp` from your `PATH` by default. Set `harn.lspPath`
in your settings if the binary lives somewhere else.

### Format on save

The extension formats `.harn` files and applies Harn autofixes when you save.
It doesn't change save behavior for other languages. To make those settings
explicit or restore them after a workspace override, add this block to your VS
Code `settings.json`:

```json
{
  "[harn]": {
    "editor.defaultFormatter": "burin-labs.harn-lang",
    "editor.formatOnSave": true,
    "editor.codeActionsOnSave": {
      "source.fixAll.harn": "always"
    }
  }
}
```

Open a `.harn` file and run **Format Document With...** once. Select **Harn
Language** if VS Code asks you to choose a formatter. Change the file's spacing
and save it. The extension should reformat the file. If Harn reports an
autofix, saving should apply it.

If formatting does not run, open **View: Output**, select **Harn Language
Server**, and confirm that `harn-lsp` started. Set `harn.lspPath` to the full
binary path when VS Code cannot find it through `PATH`.

### Cursor and other VS Code forks

Build the VSIX with the commands above. Install it with
`cursor --install-extension harn-lang.vsix`, or use **Extensions: Install from
VSIX...** from the command palette. The same format-on-save settings work in
Cursor.

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
`harness.fs.render_prompt(...)` does. Contributors changing the template language should edit
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
