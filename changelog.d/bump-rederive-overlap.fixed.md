- The Harn bump runtime now converges when the base branch changes a refreshed
  file during validation. It re-derives the refresh on the advanced base and
  publishes there, validating once per attempt, instead of failing with
  `base_conflict` after a full validation. The `base_conflict` outcome is
  removed, and the receipt adds `validated_base_oid`.
