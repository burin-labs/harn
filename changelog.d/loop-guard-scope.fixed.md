- **Loop guard break/continue scope preservation.** Harn no longer loses
  same-loop `let` bindings after an `if { break }` or `if { continue }` guard,
  preventing spurious undefined-variable runtime errors.
