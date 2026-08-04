`std/git::git_push` accepts a lease and `options.no_verify` together. Passing
both used to be denied as a bare force push, with an error naming neither: the
`--no-verify` flag moved `--force-with-lease` off the exact argv position the
reviewed-dispatch check recognized, so the push fell through to the generic
catastrophic-command floor. That combination is the canonical ref-plumbing
operation — deleting a ref you have observed at an exact OID — so the feature
did not reach the case it was written for. The argv shape now has one owner,
shared by the check that selects the reviewed dispatch and the one that
re-validates after policy hooks run.
