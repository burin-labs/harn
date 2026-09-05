Allow sandboxed Linux processes to preserve symlink metadata with
`fchmodat2`. GNU tar can now extract archives containing symlinks inside a
writable root without weakening the filesystem boundary.
