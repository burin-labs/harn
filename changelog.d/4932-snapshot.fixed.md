- Scope package snapshot reader leases to their owning VM execution so one
  parallel run cannot release another run's live package generation.
