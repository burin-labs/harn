Embedded ACP host responders no longer block message routing while binding to a
session. In-process host answers use the same deadlines as client callbacks,
including the timeout override and the exception for human input. Workspace
reanchoring during a turn takes effect for both the responder and the filesystem
on the next turn.
