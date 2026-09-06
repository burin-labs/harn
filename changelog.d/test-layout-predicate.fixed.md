`assert-outside-test` no longer fires on test suites whose layout is not `tests/` or `_test`.
A file whose stem ends in `-test` is test source wherever it lives.
A project declares its own test directories with `[lint] test_root_components`.
One typed predicate owns the layout vocabulary, so every rule gets the same answer.
