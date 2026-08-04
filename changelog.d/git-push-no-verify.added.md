`std/git::git_push` accepts a trailing `options` dict, and `options.no_verify`
skips the checkout's pre-push hook. Ref plumbing — deleting a ref, or
republishing an OID the remote already holds — pushes nothing the remote has
not already accepted, so a pre-push hook written to validate a developer's own
commits has no subject there. Without a way to opt out, every such operation
inherited the state of whichever checkout it happened to run through: its
branch, its tracking configuration, its hooks. A ref-archival push run from a
branch with no upstream failed on the hook's `@{upstream}` lookup, in a message
that named nothing the operator had typed.
