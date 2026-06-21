- **VM compiler list append optimization.** `x = x.push(item)` on local list
  accumulators now uses the same fused append bytecode as `x = x + [item]`,
  avoiding accidental quadratic cloning while preserving immutable aliasing.
