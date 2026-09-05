The session store now exposes crash-safe writer and maintenance leases, so
project-wide retention can exclude live transcript writers without heartbeats.
Automatic retention also requires an explicit session close instead of
guessing that an old open session was abandoned.
