- **Precise exfil gate (opt-in).** Under `precise_exfil_gate`, the
  lethal-trifecta exfil axis fires only on the real attack signature — the
  untrusted content controls the destination (an endpoint it named, recovered
  even from a steganographic payload), the payload ships a secret, or the
  untrusted content was flagged as a likely injection — instead of on any
  exfil-capable tool while any untrusted content is in context. Benign
  research-and-synthesis to a user-named or configured destination (a doc, a
  connector) is no longer confirmed. Destinations are matched after de-cloaking
  Unicode tag smuggling (ASCII smuggling) and zero-width / bidi host splitting,
  so a hidden exfil destination cannot slip the narrowed gate. The multi-step
  "structuring" case — a danger triangle assembled from individually innocent
  steps — is already covered: the taint ledger is context-global and persists
  for the session, so the gate fires when the exfil leg runs no matter how many
  benign steps separate it from the untrusted ingress. Default OFF (the coarse
  gate is byte-identical when disabled). The new exfil-precision battery pins
  the effect: the coarse gate confirms every benign workflow; the precise gate
  confirms none while containing every attack, including the hidden-destination
  ones.
