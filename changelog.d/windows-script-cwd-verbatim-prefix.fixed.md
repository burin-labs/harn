- **A Windows child process launched with a script-derived working directory
  now actually starts there (#7993).** `std/path` normalizes every path to
  forward slashes, so a canonicalized extended-length working directory that
  passed through a Harn script (as `workspace_root(fs)` does in
  `experiments/burin-mini`) carried its verbatim-prefix marker spelled
  `//?/C:/...` rather than `\\?\C:\...`. The prefix strip added in #7974 only
  recognized the backslash spelling, so this cwd kept its unusable prefix,
  `cmd.exe` refused it as a UNC path, and the child started in the wrong
  directory with every workspace-relative file missing. Both spellings are
  now recognized.
