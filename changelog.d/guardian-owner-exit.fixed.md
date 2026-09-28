- **A managed background command now dies when its owner exits even if another process inherited
  the owner's liveness pipe.** The guardian's reaper relays that pipe and closes it once its parent
  process is gone, so a leaked write end can no longer keep the command running.
