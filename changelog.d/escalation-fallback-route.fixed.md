Fixed escalated provider-error fallback so it restores the primary tool format with the primary provider/model instead of leaking the escalated route's tool-calling mode into the retry.
