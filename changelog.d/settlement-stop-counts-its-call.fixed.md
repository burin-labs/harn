- A call the budget ceiling refuses when it settles is now counted as the
  completed, paid call it was. The agent loop's `cost_usd`, token counts and
  run ledger (`physical_usage`, `provider_call_count`) include it, and its
  thrown error carries the call as `settled_call`. Before, every cost report
  read after such a stop omitted the call that crossed the ceiling.
