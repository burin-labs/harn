- **Provider catalog.** Together's Inkling route records its announced 2026-11-06 retirement in a
  new structured `retires_on` field. Catalog validation rejects a malformed `retires_on` or one on
  an already deprecated row. A weekly provider contract probe job files an issue when an
  announced retirement has passed or a row still selectable by alias is past its sunset;
  no pull request or main check depends on today's date (#10430).
