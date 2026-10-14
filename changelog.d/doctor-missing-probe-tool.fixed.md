- **`harn doctor sandbox` reports a case whose probe tool is missing as not
  measured, naming the tool.** A host without `perl` used to report the
  network and Unix socket cases as over-refused, which blamed the sandbox for
  a missing helper. Every probe program is now checked on `PATH` the way
  `cargo` already was.
