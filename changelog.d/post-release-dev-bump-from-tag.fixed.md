The development-identity bump that runs after a release now derives the next
version from the tag it just published, instead of from the workspace version on
the default branch. A release tagged by hand leaves that branch on the previous
development identity, which made the bump refuse and strand the branch there.
