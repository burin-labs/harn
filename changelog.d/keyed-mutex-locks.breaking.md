- **`mutex { ... }` blocks are no longer one process-wide lock.** A bare
  `mutex { ... }` now keys on its own lexical call-site, so two *distinct*
  `mutex {}` blocks run concurrently instead of silently serializing against
  each other. To guard a shared resource, name it: `mutex(resource) { ... }`
  acquires a lock keyed on the resource's structural value, so every block
  naming the same resource mutually excludes regardless of where it appears.
  Code that relied on every `mutex {}` contending on a single global lock must
  switch to an explicit shared key. Re-acquiring the *same* key on one task
  still raises `HARN-ORC-011` (self-deadlock), and locks are still released
  automatically on scope exit and on `throw`.
