- Keep recursive user-test discovery and its reported source inventory in
  agreement. Skip runtime state and generated files, honor project ignore
  rules, and preserve hidden source folders, ordinary vendor folders, and
  explicitly selected files or directory roots. Discovery errors fail the
  selection instead of silently producing an empty successful run.
