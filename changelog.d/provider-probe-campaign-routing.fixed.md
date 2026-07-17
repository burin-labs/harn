- Make `scripts/provider_tool_probe_campaign.harn --catalog-routes` plan live
  probe campaigns from canonical provider routing routes instead of raw model
  rows, so dry-run and live catalog audits target the same provider/model IDs
  as runtime dispatch.
