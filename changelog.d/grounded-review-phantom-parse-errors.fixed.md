- **Grounded-review reminders no longer mint phantom `[verified:parse_errors]`
  signals from innocent substrings in correct code.** A `look`/`read`/`search`/
  `glob` of a file whose bytes merely contain `"Parse error"` (a string literal
  or `///` doc comment) is no longer admitted as verifier output — file-display
  tools render bytes, they do not run a build/test/compiler, so they can never
  contribute a grounded review finding on a substring match alone. A passing
  test line whose descriptive name embeds an error phrase (e.g.
  `parser.test.parse error: unclosed section...OK`) is now skipped because it
  carries a trailing pass marker. Genuine verifier output — a real compiler
  `error:` parse-error line or a structured `parse_errors` array — still
  produces the grounded signal.
