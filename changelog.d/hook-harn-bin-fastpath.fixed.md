Honor exported `HARN_BIN` across git hook Harn checks so hook-spawned Make
targets reuse the same binary instead of rebuilding it.
