`harn fmt` now keeps a comment written between two call arguments inside the
call, above the argument it was written against. It used to move the comment to
the end of the file.

The comment was never lost, so a check that counts tokens could not see it. It
was simply claimed by nothing: call arguments were rendered through the plain
comma-sequence path, which has no notion of comments, and the end-of-file sweep
re-emitted every unclaimed comment after the last top-level item. A comment
explaining one argument silently came to sit at the bottom of the file.

Call arguments now go through the same claiming path that list and dict
literals already used, so any interior comment forces the call multiline and is
emitted in place. A call with no interior comments still collapses exactly as
before.
