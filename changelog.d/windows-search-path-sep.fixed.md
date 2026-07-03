`hostlib_tools_search` now reports match paths with forward-slash separators on
every platform, matching the rest of the agent tool surface. Previously Windows
emitted OS-native backslash paths (`crates\foo\bar.rs`), which shipped
non-portable paths to the model and broke path-suffix matching in downstream
tooling and tests.
