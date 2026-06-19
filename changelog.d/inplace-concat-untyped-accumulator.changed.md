- **In-place list/dict concat now also speeds up dynamically-typed
  accumulators.** The `out = out + [item]` / `out += [item]` loop already
  extended the accumulator's buffer in place (amortized O(n)) when its type was
  statically known to be a list or dict. A new fused `ConcatAssignLocal` opcode
  gates that in-place extend on the *runtime* value instead, so untyped (`any`)
  accumulators get the same O(n²) → O(n) win — and a throwing `+=` on a scalar
  now reliably leaves the binding at its previous value.
