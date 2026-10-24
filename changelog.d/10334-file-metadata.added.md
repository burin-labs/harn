`harness.fs.stat` accepts `{follow_symlinks: false}` to inspect symbolic links
without following them. Its typed result includes `is_symlink` and Unix
permission bits in `mode` and file identity in `identity`, so Harn utilities can
inspect build artifacts and recognize hard links before removing them.
