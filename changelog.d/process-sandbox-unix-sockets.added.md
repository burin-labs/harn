- **Confined children can use Unix-domain sockets under granted roots, and a
  refusal now says which boundary refused it.** `process_sandbox.unix_socket_roots`
  (`--sandbox-unix-socket-root` on `harn run`) admits bind and connect for
  socket files under the named directories and nothing over IP, so build
  servers that talk to themselves through a socket file (sbt, Gradle's Kotlin
  daemon, MSBuild worker nodes) run inside the sandbox instead of dying with a
  bare `Operation not permitted`. The grant is path-scoped and host-owned: a
  nested policy keeps only roots the outer grant covers, and backends that
  cannot filter sockets by path reject a non-empty grant rather than widening.
  Every child-process refusal record now carries `mechanism` (`egress`,
  `local_socket`, `home_read`, `write`, or `unknown`) and a `reason` naming the
  grants in force, in the event, the handler result, and the agent-visible
  error, so a refused socket or home-config read is no longer misread as a
  network denial. On macOS a loopback grant also pins the JVM to the IPv4 stack
  (`-Djava.net.preferIPv4Stack=true` in `JAVA_TOOL_OPTIONS`), because a
  dual-stack JVM binds `127.0.0.1` as `::ffff:127.0.0.1`, which the seatbelt's
  loopback filter refuses. A non-empty socket grant also admits sockets under
  the UserTemp write roots (`/tmp`, `/var/folders`), because that is where
  sbt's boot server and MSBuild worker nodes actually bind. npm and pnpm no
  longer read the denied `~/.npmrc` at startup: `NPM_CONFIG_USERCONFIG` points
  at a workspace stand-in with credential lines removed. Composer gets a
  workspace `COMPOSER_HOME` that carries `config.json` and never `auth.json`.
