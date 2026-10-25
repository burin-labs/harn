Model catalog generation now reads selected facts from a pinned Models.dev
snapshot. `harn provider catalog update-source` validates explicit upstream
routes before updating the cache; generation and runtime catalog loading use
committed data without fetching upstream. Harn retains provider behavior,
selection policy, and pricing rules
the importer cannot preserve.
