Run-tape version 5 records transcript creation and closure, allowing a served tool dispatch to validate a child created
and closed entirely within it. Missing targets, malformed changes, and incomplete lifecycles refuse. Read-only Git
conflict and remote-ref queries also reach recorded-command replay.

Migration:

Rust consumers match `TranscriptChange::Fields` or `TranscriptChange::Lifecycle` instead of accessing struct fields.
Version 4 field-change records retain their existing wire shape and remain readable; consumers must support version 5
before reading new recordings with lifecycle records.
