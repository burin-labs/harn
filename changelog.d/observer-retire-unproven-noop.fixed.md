Closing a pull request that was reopened, or whose head is a fork, no longer fails the
consumer observer cleanup job. It logs that nothing was retired. Retirement still needs a
proven closed same-repository candidate, and a contradictory read stays a red failure.
