- Foreground Rust embedding checks reuse the test build profile and run in the lighter CI test partition,
  reducing repeated compilation while retaining both executable variants and the initialization controls.
  Linux and macOS embedding builds honor their existing measured compiler budgets.
