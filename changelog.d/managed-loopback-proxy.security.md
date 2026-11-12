On macOS, granting local TCP listeners no longer lets a child bypass its managed network proxy with direct localhost
connections. Local bind and inbound permissions remain available, and runs without a managed proxy keep their existing
loopback access.
