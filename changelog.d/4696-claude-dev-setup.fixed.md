- **Claude dev setup hook JSON handling now runs through Harn (#4696).** The
  startup hook no longer embeds Python for hook input parsing or SessionStart
  context rendering, while avoiding implicit Harn rebuilds during bootstrap.
