- **Debugging and test-result edge cases.** Conditional breakpoint fallback
  evaluation now reports unknown or non-numeric conditions instead of silently
  firing, JUnit duration parsing saturates hostile timestamps instead of
  panicking, HTTP download byte counts no longer wrap, and `to_int` now follows
  the documented bool and invalid-float behavior.
