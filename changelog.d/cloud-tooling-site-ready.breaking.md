`harn serve site --ready-file PATH` atomically publishes the actual bound URL
and process ID as JSON. `SiteHttpServeOptions` adds the optional `ready_file`
field for Rust callers.

Migration: Add `ready_file: None` to existing `SiteHttpServeOptions` struct
literals, or set it to a new receipt path when the launcher needs that endpoint.
