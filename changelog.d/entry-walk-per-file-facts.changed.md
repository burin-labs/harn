Preparing many entries in one process now parses and hashes each imported file
once per file version instead of once per entry that imports it. Entry cache keys
fold in each imported file's digest rather than its text, so existing entry cache
records are recompiled once.
