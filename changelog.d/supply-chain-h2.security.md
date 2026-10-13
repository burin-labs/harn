- Update the locked `h2` HTTP/2 dependency to 0.4.20, which patches RUSTSEC-2026-0258
  (empty DATA frames queued without limit). It reaches Harn through the AWS SDK clients.
