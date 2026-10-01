Trusted process inference ceilings now survive isolated API sessions,
restored and forked ACP sessions, and shared server dispatch. Harn captures
the ceiling at launch, meets it with tighter session limits, and withholds
the reserved value from ordinary child processes. Malformed launch policies
refuse before provider configuration seeding or server connections.
