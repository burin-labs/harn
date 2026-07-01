**Verify/build/test subprocesses now run under a deterministic English message
locale.** Spawned tool commands inherited the user's shell locale, so a
non-Anglosphere user whose environment set `LC_ALL`/`LANG` to a localized value
got translated (non-English) compiler and test output — silently breaking every
downstream matcher that keys on English diagnostics (deterministic syntax
repair, error-signature grounding, completion/pass-fail classification). Both
spawn paths (`process.exec` builder and the `harn-hostlib` real spawner) now
strip an inherited `LC_ALL` and pin `LC_MESSAGES=C` plus `DOTNET_CLI_UI_LANGUAGE=en`
(the .NET CLI ignores `LC_*`), while deliberately leaving `LC_CTYPE`/`LANG`
untouched so UTF-8 handling of non-ASCII source is preserved. An explicit
caller-supplied `env`/`env_remove` still wins, matching the `TMPDIR` overlay.
