- Reopening a saved session replays each tool call under its real name. Replay now reads the name from the
  event's `metadata.tool_name`, where tool lifecycle events store it, instead of labelling the call `tool`.
