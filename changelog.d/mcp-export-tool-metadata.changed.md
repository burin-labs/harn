`harn serve mcp` no longer pretends every exported tool is destructive and
open-world. Tool titles and descriptions come from the `pub fn` doc comment,
behavior hints come from `@annotations(...)`, and the script's leading doc
comment is served as MCP `instructions`. A sibling `<script>.md` is exposed as
`harn://package/howto`. Undeclared hints stay off the wire.
