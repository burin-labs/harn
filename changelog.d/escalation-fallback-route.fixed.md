Fixed escalated provider-error fallback so it restores the primary provider,
model, and tool format instead of leaking the escalated route's tool-calling
mode into the retry.
