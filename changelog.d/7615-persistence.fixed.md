- **Automatic execution evidence persistence (#7615).** The canonical run path now
  uses one Harn-owned VM interface for automatic run records, bounded retention,
  and optional flight artifacts. Embedded hosts can reuse the same behavior, and
  a durable flight-artifact receipt survives a later run-record write failure.
