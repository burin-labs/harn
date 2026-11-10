An ACP `session/remind` sent while a session's prompt is running now reaches that prompt. The
server answered it only after the prompt returned, so an `interrupt_immediate` reminder could never
wake a daemon agent loop idle inside its prompt, whose prompt returns only when the session stops.
