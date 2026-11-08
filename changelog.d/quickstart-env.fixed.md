Quickstart excludes its local `.env` from Git and writes it with owner-only
permissions on Unix. It preserves existing ignore rules and refuses tracked
environment files or symlink destinations before changing configuration.

Shared atomic writes apply requested or preserved Unix permissions when the
temporary file is created, before writing its contents.
