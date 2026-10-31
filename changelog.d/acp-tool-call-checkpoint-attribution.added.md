- **Opted-in ACP sessions attribute each tool call's file changes automatically.**
  With `workspaceCheckpoint: true`, the agent loop now brackets every admitted
  tool call with the checkpoint `call_begin`/`call_end` window, keyed by its tool
  call ID and recorded with the tool's declared side-effect class. A restore plan
  for an earlier message names the files those calls changed without the client
  bracketing calls itself. Read-only calls and calls without an ID are not
  bracketed.
