- **Same-App repairs survive a bump refresh.** The bump driver treated any
  commit signed by GitHub for its own App as its refresh output, so a repair
  created through the same App credentials was discarded on the next refresh.
  A commit is now refresh output only when it also carries the exact
  `chore: bump Harn runtime to vX.Y.Z` headline, and a refresh refuses to
  publish under any other headline.
