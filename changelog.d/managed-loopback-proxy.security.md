On macOS, granting local TCP listeners no longer lets a child bypass its managed network proxy with direct localhost
connections. Managed proxy grants admit only the actual IPv4 listener endpoints, excluding unrelated IPv6 services on the
same port numbers. Local bind and inbound permissions remain available, and runs without a managed proxy keep their
existing loopback access.
