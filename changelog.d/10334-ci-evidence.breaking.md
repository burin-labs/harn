CI test inventory, completion receipts, and workspace build evidence now use Harn.
The shell callers reuse an explicit Harn binary or the verified bootstrap seed,
so they work before a source CLI is built. Completion requires a nonempty,
consistent inventory and one terminal event per selected test. Build evidence
reads fingerprint hash markers and file sizes without opening fingerprint JSON.

Migration: The CI-only language-server fixture switch `HARN_LSP_FIXTURES_REQUIRED` is now
`HARN_EXT_LSP_FIXTURES_REQUIRED`. Update local exports of the old name. The
extension prefix keeps this test-runner setting separate from runtime settings,
so older bootstrap releases can run while CI still requires the language servers.
