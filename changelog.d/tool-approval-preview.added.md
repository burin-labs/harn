- `tool_define` accepts an `approval_preview` closure that maps a call's
  arguments to `{command, cwd?, summary?}`. When Harn asks the host to approve
  the call, it sends this record as a `command_preview` evidence ref, as an ACP
  text block in `toolCall.content`, and at `toolCall._meta.harn.approvalPreview`.
  A tool whose arguments don't name its command, such as a no-argument
  `verify`, can now show the command a person is approving. A preview that
  throws or is malformed is omitted, and it never changes the decision or
  `rawInput`.
