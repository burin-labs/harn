- **Repository policy checks on pull requests.** The "Check repository policies" job now runs on every
  pull request and merge group that changes more than docs, instead of only after a merge to main,
  so a change that breaks a policy is refused before it lands.
