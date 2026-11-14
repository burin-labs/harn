A session that grants `PATH` keeps the SwiftPM and pnpm sandbox launchers ahead of it,
so `swift build` in a confined command no longer fails on SwiftPM's nested sandbox.
