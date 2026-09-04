- **A canonical session store stops being watched once nothing has it open
  (#7960).** Opening a canonical store added its file to a process-wide list
  that was never pruned, so a process that opened many stores over its life
  attached a SQLite reader, a filesystem watcher and a thread to every database
  it had ever touched, including ones whose handle was long gone. The claim now
  belongs to the store handle: the watcher for a path starts while a handle is
  open, and is stopped and joined when the last handle drops.
