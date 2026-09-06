The linked-program graph binding is now covered by tests for the case that
matters when a compiled artifact is suspected of being stale: a source edited
in place, keeping its byte length and its exact modification time, is refused
against the digest recorded when the program was linked, and so is a source
that no longer parses. The binding hashes source content, so neither can pass,
and the refusal names the mismatch.
