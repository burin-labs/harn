- Report free disk space correctly on Windows. `harn doctor`/hardware detection
  canonicalized the probe path (which yields an extended-length `\\?\` verbatim
  path on Windows) and then matched it against sysinfo's plain `C:\` mount
  points; the verbatim prefix never prefix-matched, so free space read as
  unavailable on every Windows machine. The path is now de-verbatimed before the
  volume comparison.
