# Git repository identity

The hostlib `tools.git_repository_identity` method takes `repo`, a directory
inside a working tree. It returns `worktree_root` and
`common_directory` as absolute paths. Linked worktrees have different
working-tree roots and the same common directory. Unrelated repositories have
different common directories.

The operation invokes system Git with fixed arguments, without a shell or
caller-supplied command. A missing Git executable, a non-repository directory,
a bare repository, or an incomplete response returns an error. Consumers must
keep a failed observation distinct from a selected project root. This read
does not grant permission to execute a verifier or modify the repository.
