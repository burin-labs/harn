- **Cross-agent zero-trust (opt-in).** Under `authenticate_directives`,
  `classify_result_trust` now distrusts a result returned over a delegation /
  A2A channel by ORIGIN — a tool annotated with an `agent_channel` capability —
  rather than by a forged-authority keyword vocabulary. A peer agent's output
  may itself have ingested untrusted content, so it is quarantined as untrusted
  data and cannot smuggle authority regardless of phrasing; provenance-stamped
  hand-offs still authenticate. The containment battery shows this lifts
  cross-agent-poisoning containment from 1/10 (keyword authenticator) to 10/10
  and overall exfil-sink containment from 0.49 to 0.63 under the opted-in
  posture, with the default posture byte-identical.
