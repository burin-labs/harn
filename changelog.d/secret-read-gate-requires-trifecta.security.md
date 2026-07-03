- **Secret-read gate requires the trifecta gate.** `SecurityPolicy::from_config`
  now gates `gate_secret_reads` on `trifecta_gate`. The secret-read arm is
  evaluated only inside `trifecta_gate_reason`, which runs solely when the
  trifecta gate is armed, so it is inert on its own. Gating it on its
  prerequisite (mirroring the precise-exfil gate) removes the dead
  "secret-read gate on, trifecta gate off" configuration. Default posture is
  byte-identical.
