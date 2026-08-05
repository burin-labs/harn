The release-provenance gate no longer fails on a reviewed bump of the
attestation action. It asserted `actions/attest` at one exact commit SHA, so
every legitimate update to that action broke the gate until someone edited the
assertion to match — and an assertion that must be edited in lockstep with the
thing it asserts cannot fail for any reason except forgetting to edit it.

It now requires `actions/attest` pinned to a full-length commit SHA, which
rejects the failures that actually matter — a mutable tag such as `@v4`, or a
different action entirely — while letting a reviewed version change through.
