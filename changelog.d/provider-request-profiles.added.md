- **Provider tool-probe audits now cover request profiles (#4192).** `harn
  provider tool-probe-audit` validates both catalog-default and
  parameter-edge request shapes offline, and `harn provider tool-probe
  --dry-run-request` can render a selected profile without making a provider
  call.
