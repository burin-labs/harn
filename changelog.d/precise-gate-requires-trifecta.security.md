- **Precise exfil gate requires the trifecta gate.** `SecurityPolicy::from_config`
  now gates `precise_exfil_gate` on `trifecta_gate` structurally. The precise
  gate only narrows the coarse trifecta gate — its logic runs solely inside
  `trifecta_gate_reason`, which is called only when the trifecta gate is armed —
  so it is inert on its own. Gating it on its prerequisite (mirroring the
  existing file/command-provenance gate) means the nonsensical "precise gate, no
  trifecta gate" configuration can no longer arise from config or a future
  caller. The live install path routes through `from_config`, so the invariant
  holds end-to-end. Default posture is byte-identical.
