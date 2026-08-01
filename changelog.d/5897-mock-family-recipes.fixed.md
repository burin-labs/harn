`harn fix` now tells callers where the HTTP mock family and the egress-policy
declaration went. `http_mock`, `http_mock_clear`, `http_mock_calls`, and
`egress_policy` are hand-written methods rather than registered builtins, so no
recipe derived for them and upgrading packages saw a bare "not defined" error on
every test file instead of a repair.
