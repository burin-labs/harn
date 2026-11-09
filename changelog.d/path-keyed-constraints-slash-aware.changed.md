Path-keyed tool argument constraints now match slash-aware, like a shell or
`.gitignore`: a key the tool declares in `arg_schema.path_params` is resolved to
its workspace-relative spelling, `*`, `?` and `[...]` stay within one segment,
and `**` crosses directories. `src/*` no longer admits `src/nested/a.rs` (write
`src/**`), `src/**` no longer admits `src/../outside.rs`, and an array path
argument is admitted only when every entry is. Non-path keys such as a command
keep flat-name matching. `glob_match` uses the same path matcher, and path globs
across Harn gain `[...]` classes and `{a,b}` alternates.
