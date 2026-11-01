The crystallization curator's `scaffold_harn_tool` accepts `steps`, a learned command sequence, and returns a complete,
tested MCP tool that runs it in order with failure receipts. It also returns the same sequence as one shell line
(`artifact.verify_command`) that a host can adopt as the project's verifier. The renderer is
`crystallization_command_sequence_package`.
