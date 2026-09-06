The Go protocol binding's round-trip test no longer compares the fixture
against the removed `ArtifactVersion` constant, and the release gate no longer
passes a version to the protocol-artifact check. Both were left behind when the
version stamp was dropped from the generated artifacts.
