- **Cookie and stateless-session helpers now use the maintained `cookie` crate
  for RFC parsing, serialization, and signing (#5542).** Signing secrets must
  contain at least 32 bytes of cryptographically random data, signed values use
  the crate's authenticated wire format, and `SameSite=None` serialization
  automatically adds `Secure`.
