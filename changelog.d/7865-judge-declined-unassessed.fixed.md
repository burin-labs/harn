- **A completion judge that refuses without itemizing keeps its own reason
  (#7865).** When the judge declines a turn and returns no per-requirement
  assessment, the decision now carries the judge's stated reason instead of
  overwriting it with a pending-requirements message that blames the actor for
  work the judge never assessed.
