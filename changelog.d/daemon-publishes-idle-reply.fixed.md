A daemon agent loop now publishes the reply it goes idle on. A toolless reply was published only
when its session finalized, and a daemon idles between replies instead of finalizing, so ACP clients
saw no `agent_message_chunk` for any of a daemon's replies until the session ended. A reply that
wake input supersedes before the daemon idles stays unpublished.
