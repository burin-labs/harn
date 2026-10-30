Use serial Rust front-end compilation on the measured 3-core, 7-GiB GitHub-hosted Mac to reduce swapping during
large workspace test builds. Other hosts and explicit compiler settings retain their existing behavior.
Keep Mac warm artifacts separate by compiler wrapper and effective flags so different hosted hardware can reuse its
own compiled workspace outputs.
