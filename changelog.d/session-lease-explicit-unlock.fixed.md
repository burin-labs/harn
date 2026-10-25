Session writer, task and maintenance leases unlock explicitly on drop, so a process spawned by another
thread can no longer keep a released session lease locked and refuse the next run.
