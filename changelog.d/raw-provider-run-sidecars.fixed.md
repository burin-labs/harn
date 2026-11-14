- **Saved run transcripts retain their referenced raw provider files.** Moving a
  run bundle preserves captured requests, responses, and transport failures.
  Missing, malformed, or symlinked artifacts mark the transcript unavailable
  without changing the model answer or run outcome. Failed snapshots no longer
  reuse an older transcript as verified evidence.
