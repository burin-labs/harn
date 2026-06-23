- **Streaming text tool-call promotion now reports parsed arguments as raw
  input.** Promoted candidate events populate `rawInput` instead of mislabeling
  arguments as `rawOutput`, preserving event-log provenance for tool-call
  forensics.
