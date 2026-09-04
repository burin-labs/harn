- **A confined agent command on Windows can see the machine's installed
  toolchains again (#7993).** Commands run inside the sandbox launch in an
  AppContainer, whose token is access-checked twice: once against the user
  and groups it carries, and once again against its AppContainer and
  capability entries. A directory whose permissions grant only `Everyone` or
  `BUILTIN\Users` passed the first check and failed the second, so an
  interpreter installed under `Program Files` was unreadable and the shell
  reported it as "not recognized" — indistinguishable from it not being
  installed. The child's token now carries the groups that hold those default
  read permissions, so reads resolve through the permissions the machine
  already has. Writes are unchanged: they are held inside the workspace by
  the Low integrity level, which is checked before permissions and ignores
  the added entries.
