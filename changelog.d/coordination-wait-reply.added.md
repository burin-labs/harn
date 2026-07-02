- **Stdlib coordination request/reply helpers.** `std/coordination` now includes
  `coord_request` and `coord_wait_reply` so harnesses can build durable
  addressed request/decision protocols with request-scoped acknowledgement
  cursors instead of product-local mailbox glue.
