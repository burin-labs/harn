`harn test --coverage-out <path>` now always writes the LCOV tracefile, even
when no on-disk source executed. An explicitly requested artifact is no longer
silently skipped on an empty report (which would break a CI step that consumes
the file); the empty report renders to a valid zero-record LCOV.
