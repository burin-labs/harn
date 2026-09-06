Release recovery can publish the certified candidate's existing signed archives
from current release tooling without rebuilding them or selecting a newer source
commit. Explicit publication inputs bind the immutable tag, source, producer,
manifest digest, and policy revision before release assets or crates are published.
Main-push release checks now recognize the certified-tag wait state without
failing, and advance to the next development version only after the GitHub
Release is observably published.
Promotion authenticates the signed durable candidate-to-run binding before it
downloads any archive, and publication no longer mints a contents-write token
for its read-only trusted-tag path.
