- **Harn CI audit gate wall-clock.** The conformance and independent audit-gate
  fan-out now share one warmed `harn` binary and run in parallel, preserving the
  same coverage while reducing merge-queue critical-path time.
