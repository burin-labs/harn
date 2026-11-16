A release whose commits pair with two consumer pull requests can now be promoted
through one explicit consumer pull request, but only when the revision it
rehearses contains every paired pull's head. A pull that is not contained
refuses by name, and the rehearsal binding records the superseded pulls.
