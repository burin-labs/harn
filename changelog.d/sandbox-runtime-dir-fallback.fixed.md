A confined child keeps a working temp dir and toolchain cache when the sandbox runtime dir cannot be created, such as
in a container with `HOME` unset or unwritable. It warns and uses the self-ignored `.harn-tmp` and
`.harn-toolchain-cache` in the first workspace root. The runtime dir is now named `.harn-sandbox`, so a project's own
`harn-sandbox` directory is no longer skipped by file walks.
