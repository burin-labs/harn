- **Recording spend no longer re-indexes the whole session.** A session metadata update now
  rebuilds search rows only when the title, cwd, model, or project scope changes. Every turn end
  records spend this way, so a stopped turn on a loaded machine no longer waits on one embedding
  per transcript event before its reply.
