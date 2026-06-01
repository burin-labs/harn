- **Faster local-variable reads.** `GetLocalSlot` no longer clones the
  binding's name `String` on every successful read — that per-instruction
  heap allocation, used only to build a cold error message, is now deferred
  to the error path. This removes the dominant allocation in tight
  local-variable loops with no behavioral change.
