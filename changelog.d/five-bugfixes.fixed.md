- **Policy, parser, host process, and OAuth edge cases are now handled more
  strictly.** Unix-socket JSON requests and provider file uploads now
  participate in network/file policy gates and handoff effects, malformed
  loopback OAuth requests no longer abort a pending valid callback, background
  command handles preserve unavailable process-group IDs as `nil`, background
  feedback peeks no longer restamp unrelated inbox entries, generic and
  `where` lists reject empty/trailing-comma forms.
