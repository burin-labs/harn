The session store now exposes crash-safe writer and maintenance leases, so
project-wide retention can exclude live transcript writers without heartbeats.
Persisted open status remains independent of writer liveness, so dormant
resumable history remains eligible for automatic retention.
