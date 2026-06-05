# Provider Capability Sources

These TOML fragments are the editable source for Harn's built-in provider/model
capability matrix. The runtime embeds `../capabilities.toml`, which is generated
from this directory by:

```sh
harn providers build-capabilities
```

Use `harn providers build-capabilities --check` to verify that
`../capabilities.toml` matches the fragments. Direct edits to
`../capabilities.toml` are invalid because the next generation pass will
overwrite them.

Fragments use the same `CapabilitiesFile` schema as `[capabilities]` project and
package overlays. File names are sorted lexicographically; keep numeric prefixes
when adding rules because capability matching is first-match-wins.
