- A release candidate is now refused when the bump driver at harn-bump-fleet's promoted
  orchestration commit does not type-check against it. Publishing such a candidate failed
  every consumer's runtime bump, as v0.10.153 did.
