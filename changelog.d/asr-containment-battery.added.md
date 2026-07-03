- **Lethal-trifecta containment battery.** `security::battery::run_containment_battery`
  drives the malicious ASR corpus through the lethal-trifecta gate, model-free
  and deterministic, and reports per-class containment: does each attack's
  ingress register taint so a fully-obeyed exfiltration attempt is forced to
  confirm? It measures the product-level guarantee — *even a fooled model is
  contained* — that the detection tier alone cannot show. The pinned baseline
  (default posture) contains network-boundary ingress (`web_fetch`, mounted MCP)
  but exposes the honest residual: subagent/A2A channel messages register no
  taint, so cross-agent poisoning is uncontained unless directive
  authentication is enabled — and even then the current marker vocabulary
  catches only canonically-framed forged authority.
