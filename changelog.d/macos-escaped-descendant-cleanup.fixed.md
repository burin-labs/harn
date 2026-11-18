- **macOS command cleanup kills a descendant caught mid-exec.** A command's
  escaped descendant whose environment was unreadable because it was between
  exec images (as a framework Python is right after it starts) was skipped by
  the cleanup kill, found alive afterwards, and turned a successful command
  into a failed one. It is now re-read and killed. An exited orphan that
  launchd has not yet reaped is also no longer counted as a survivor.
