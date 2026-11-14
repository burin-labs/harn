OpenAI Responses stream failures retain their phase, partial-response, and deadline facts.
An eligible interruption before the first parsed response event can use the existing one-shot
fallback to a non-streaming request. Parsed events, including metadata without visible text,
and deadlines prevent automatic fallback.
