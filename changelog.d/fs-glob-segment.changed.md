`harness.fs.glob` now matches `*` within a single path segment, as a shell
does; use `**` to cross directories. Previously `glob("*.txt")` also returned
`sub/x.txt`.
