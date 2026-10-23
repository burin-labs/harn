CI test inventory, completion receipts, and workspace build evidence now use Harn.
The shell callers reuse an explicit Harn binary or the verified bootstrap seed,
so they work before a source CLI is built. Completion requires a nonempty,
consistent inventory and one terminal event per selected test. Build evidence
reads fingerprint hash markers and file sizes without opening fingerprint JSON.
