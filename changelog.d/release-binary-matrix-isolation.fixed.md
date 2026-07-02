- **Release binary builds no longer cancel healthy platform legs.** The
  release-binary workflow now lets every target finish even when one target
  fails, preserving incrementally uploaded archives and making recovery reruns
  fill only the genuinely missing assets.
