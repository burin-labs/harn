Keep valid capability bundles intact during runtime bumps when they carry more
capabilities than an imported helper requires. The migration no longer inserts
a duplicate argument before such bundles.
