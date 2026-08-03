`host_has` now reports the host operations answered by the active
`harness.testing` fixture scope. A script that gates its host call on the
capability manifest previously skipped the call and never reached its fixture,
so the retired `with_host_mocks` wrapper — which merged its mocks into the
manifest — could not be replaced by `with_capability_fixtures` without silently
losing the behavior under test.
