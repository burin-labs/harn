- **Confined JVM commands retain launcher options (#11449).** Isolated and granted sessions preserve `JAVA_TOOL_OPTIONS`,
  including a caller's private Java temporary directory, while keeping ungranted credentials out of child processes.
  Confined JVM commands without an explicit Java temporary directory use the same private temp directory as other tools.
