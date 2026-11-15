- A process sandbox read, write or socket root reached through a link that
  sits in a directory a confined child can write is no longer granted, and a
  write or socket root on or over the user's git, gh or ssh configuration is
  not granted either. Each refusal is reported once.
