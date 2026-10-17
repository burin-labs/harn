Responses request encoding and its deterministic wire tests now live in the
shared LLM crate, reducing the Native runtime's compilation work while retaining
catalog decisions, transport, and audit delivery in the runtime.
