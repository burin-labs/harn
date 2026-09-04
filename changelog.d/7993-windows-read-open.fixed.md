- **Windows agent commands can see the machine's toolchains again (#7993).**
  A confined child on Windows runs inside an AppContainer, and an AppContainer
  is a LowBox token: every access check also runs against the container's own
  package SID, its capability SIDs, and `ALL APPLICATION PACKAGES`. Read access
  therefore has to be granted with an explicit ACE, and the grant list was both
  narrower than the "reads are open, writes are confined" contract and gated on
  a sandbox preset an embedder could simply forget. A command as ordinary as
  `node --version` failed, and `cmd` reported the unreadable executable as
  "'node' is not recognized", so it read as a missing toolchain rather than a
  denied read. The toolchain read grants now apply whether or not the policy
  carries the `DeveloperToolchains` preset, and every directory on the parent's
  `PATH` that lives under the user profile is granted read and execute as well.
  Writes are unchanged: they still reach only the workspace roots the policy
  names. Package-manager config roots, which include credential files, stay
  behind the `PackageManagerConfig` preset.
