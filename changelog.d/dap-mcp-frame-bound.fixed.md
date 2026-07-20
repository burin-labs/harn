- **MCP JSON-RPC stdio framing now bounds `Content-Length` before allocating.** A peer (or a launched MCP
  subprocess) that announced an oversized `Content-Length` header previously drove an unbounded
  `vec![0; length]` allocation in `harn-serve`'s stdio transport before a single body byte arrived, so a
  hostile or malfunctioning peer could exhaust memory. The reader now rejects any frame larger than 16 MiB at
  header-parse time. The `harn-dap` debug adapter's `Content-Length` framing — previously reimplemented six
  times across the crate — is consolidated into one internal `framing` module that enforces the same bound on
  every read path, matches header names case-insensitively, and rejects a malformed or overflowing
  `Content-Length` instead of silently ignoring it.
