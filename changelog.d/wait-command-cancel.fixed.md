`session/cancel` now ends a turn that is waiting in `tools.wait_command`. The wait used to block
the session thread, so the cancel went unread until the wait timed out.
