- Run-tape branches replay reminder revocation and expiry state alongside
  transcript changes. Later revocation requests retain their recorded status;
  incompatible initial state refuses before file or transcript effects.
  Unaccounted reminder retirements make the dispatch unservable.

  Migration: Rust consumers must handle the new `TranscriptChange::Reminders`
  variant in exhaustive matches. Record a new full tape in format 7 for exact
  branching. Older tapes remain readable for inspection.
