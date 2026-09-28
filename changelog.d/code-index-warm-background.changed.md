- The session code-index warm thread now runs at background priority (utility QoS
  on macOS, nice 10 on Linux), so a rebuild no longer competes for the CPU with
  the engine thread preparing the first model call.
