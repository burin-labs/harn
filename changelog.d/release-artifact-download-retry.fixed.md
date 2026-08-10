Release-critical artifact restores now retry bounded transient GitHub service
failures while preserving the official downloader's terminal integrity checks.
Candidate manifests also preserve each target's producing attempt, so rerunning
only a failed target can reuse verified artifacts from earlier attempts.
