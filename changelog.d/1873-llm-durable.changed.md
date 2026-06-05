- **Durable LLM rate-limit admission.** Catalog and runtime LLM RPM/TPM
  limits now use shared SQLite admission by default across Harn processes, so
  parallel eval runners and worker fleets respect one provider/model quota
  without relying on per-process sleeps or ad hoc environment-only guardrails
  (#1873).
