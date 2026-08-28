Keep loopback-only subprocess grants narrow even when the surrounding agent
may use network-capable tools; unsupported platforms reject the grant instead
of widening it to unrestricted sockets.
