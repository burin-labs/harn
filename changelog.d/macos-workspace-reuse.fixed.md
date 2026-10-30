macOS workspace CI can reuse unchanged crates from the successful main build instead of recompiling the entire workspace
after every checkout. Changed sources and their dependents still rebuild, and each run publishes compiler timings.
