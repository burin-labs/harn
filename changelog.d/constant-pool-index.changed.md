- **Compiler constant-pool building.** The bytecode compiler now indexes
  constants while emitting chunks, avoiding quadratic duplicate scans in
  generated scripts with many literals.
