Pushes to the main branch no longer cancel the CI run before them. Every main
push shares one concurrency group, so cancelling on push meant each merge
superseded the run in flight; under a steady merge rate the branch stopped
completing runs, and a head commit whose run was cancelled carries no executed
result at all. Main runs now queue behind each other. Pull-request runs still
cancel on a new commit.
