- **Grounded-review context no longer leaks terminal escape sequences.** Captured tool
  output forwarded into LLM context was cleaned by a hand-rolled scanner that only
  understood `ESC [` CSI sequences terminated by an ASCII letter, so OSC (`ESC ]`,
  e.g. window-title sequences), DCS, and 8-bit CSI introducers passed through verbatim.
  Stripping is now done by a real VT parser (`strip-ansi-escapes`). Stripping also runs
  *before* the byte budget is applied rather than after, so escape bytes no longer
  consume the budget or get cut mid-sequence.
