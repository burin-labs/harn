Restored the documentation sidebar nesting for the language specification. The
new platform support entry was inserted directly above the generated chapter
list, and because that list is indented, mdBook re-parented all 33 specification
chapters under "Platform support". `sync_language_spec.harn` now pins the entry
the generated block belongs to and fails when something else takes it, so the
sync check owns the nesting rather than only the contents between its markers.
