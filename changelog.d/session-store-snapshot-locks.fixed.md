Take read-only session-store snapshots through SQLite's online backup API instead of
copying the database, `-wal`, and `-shm` files. The plain copy closed descriptors on
files this process had open through SQLite, which released its POSIX locks; another
connection could then truncate a wal-index still mapped here and crash the process
with SIGBUS.
