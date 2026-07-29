`harn test` now supports named table cases and typed `@test_fixture(scope: file|case)` setup. Rows and fixture
values use ordinary callable checks, file-scoped data is copy-on-write isolated per case, and fail-fast prevents
queued cases or later fixture setup from starting after the first failure.
