- `edit_rename_symbol` (`code_index.rename_symbol`) asks rust-analyzer for Rust renames, so renames through
  derives, macro calls, `macro_rules!` bodies, rustdoc links and paths into other workspace crates succeed
  instead of refusing. It falls back to the syntax-tree engine when the server is missing or still loading,
  reports the engine in a new `backend` field, and accepts `backend: "auto" | "language_server" | "syntax_tree"`.
