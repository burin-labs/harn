An MCP tool that reaches an unsupported host operation now names the operation
it could not run. The refusal was raised as a thrown value, and a thrown value
is redacted to "tool threw an undeclared value" on the way out to the caller,
which hid whether the manifest had reached runtime dispatch at all.
