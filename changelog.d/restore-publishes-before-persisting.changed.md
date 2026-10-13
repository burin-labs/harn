Restoring the code index from a snapshot now publishes the reconciled index
before rewriting the snapshot, so a restart after edits is ready sooner. The
write runs in the background; `CodeIndexCapability::wait_until_idle` waits for
it, as it already did for a background warm, and embedders that exit right
after a restore should call it first.
