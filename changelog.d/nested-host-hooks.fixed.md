Registered host hooks can run for child agents started inside a tool handler.
Hooks retain their registration context, so tool-registered hooks remain restricted
and the parent tool's restrictions resume after the callback, including when the hook fails.
