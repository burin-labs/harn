Git hooks now keep commits and pushes responsive by default: local hooks retain
cheap formatting, signature, merge-queue, and drift guards while required CI
owns build-backed validation. Set `HARN_HOOKS_FULL_LOCAL=1` to opt into the full
local gate when needed.

The ported-handler LOC ratchet now runs inside the already-warm required audit
lane instead of paying for a separate Rust checkout and cold Harn build.
