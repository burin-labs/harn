# HARN-SUS-007 — ResumeConditions trigger could not be registered

The `conditions.trigger` entry was valid enough to request an auto-resume
binding, but Harn could not register or resolve the trigger in the live
dispatcher registry.

Check the trigger kind, event matchers, and registry availability. Prefer
normalizing the full `ResumeConditions` table before suspension so shape
errors surface as `HARN-SUS-002`.
