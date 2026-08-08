- Killed the remaining UTF-8 byte-slicing panics of the class fixed for
  diagnostic excerpts in #6328: `url_decode` no longer aborts on a `%`
  followed by a multi-byte character (it now shares the byte-based
  `percent_decode`, which also stops accepting signed escapes like `%+A`),
  transcript-compaction tool-result previews cut on character boundaries,
  and the Ollama NDJSON stream-error and tool-argument parse paths quote
  malformed frames boundary-safely instead of panicking while reporting
  them.
