- **Durable spend receipts distinguish unsettled and unpriced attempts.** Strict
  scopes refuse new calls when an earlier attempt is unsettled or its usage is
  unknown.

  Migration: downstream code constructing `MachineSpendReceipt` with a struct
  literal must provide `lifetime_unsettled_attempts` and
  `lifetime_unpriced_attempts` from its spend accounting.
