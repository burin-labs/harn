Make `with_redaction` rewrite only the recorded arguments and the model-visible result. Handlers now always receive the
original arguments; use a custom middleware that calls `next(call + {tool_args: ...})` to change them.
