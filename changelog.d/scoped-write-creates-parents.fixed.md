Restored the `mkdir -p` contract for content-producing filesystem writes.
The scoped-write hardening first shipped in v0.9.18 (#4147) resolved parent
directories at open time via a symlink-safe parent-fd walk, but that walk
required every ancestor directory to already exist. As a result, `write_file`,
`write_file_bytes`, `append_file`, `harness.fs.write_text`, and `http_download`
could fail with "No such file or directory" when writing into a not-yet-created
directory. These writes now recreate missing ancestor directories in the
sandboxed path, unrestricted path, and test overlay fallback. Structural
operations (copy, move, remove, single `mkdir`) keep their "parent must already
exist" semantics.
