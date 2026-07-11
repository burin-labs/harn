- Added `std/pii` — structured PII detection and reversible redaction. `pii_detect` finds email, phone,
  US SSN, credit-card (Luhn-validated), IBAN (mod-97-validated), IPv4, and IPv6 entities with character spans;
  `pii_redact` replaces them with stable placeholder tokens (`<EMAIL_1>`) and `pii_restore` reverses the mapping,
  for the redact-before-model / restore-after-model harness flow. Pure-`.harn` regex packs over the existing
  `regex_captures` seam; NER-based name/address detection is a documented future extension.
