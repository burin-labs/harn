Reading an environment variable inside a confined session no longer creates
files. Each read used to build the whole child-process environment, which on
macOS rewrote the SwiftPM launchers and toolset under `.harn-toolchain-cache`.
A provider catalog reads the environment once per provider, so agent steps
spent most of their time in that disk work. Reads now compute the same
workspace values without touching disk; spawns still create what a child needs.
