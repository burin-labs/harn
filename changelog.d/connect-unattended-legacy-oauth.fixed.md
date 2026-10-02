- `harn connect <provider>` treats an explicit `--redirect-uri` as explicit even
  when it equals the loopback default. Before this change, recovering a legacy
  OAuth registration without a terminal failed with "did not record its
  redirect URI" even though the URI had been passed.
- OAuth connect commands accept `--client-secret-from-env NAME` and
  `--client-secret-file PATH`. When a legacy confidential client needs its
  secret again and no terminal is attached, the error now names these flags.
  Before this change it failed with "Device not configured".
