- harn-lsp adds a `harn.applyRepair` `workspace/executeCommand` that resolves a
  code action's `repair_id` into a `WorkspaceEdit` on demand, so editors can
  apply repair-backed fixes that ship without an inline edit.
