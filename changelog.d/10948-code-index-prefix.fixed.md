A run-tape branch now replays the prefix of an agent that warms the code index before its first model call.
Code-index maintenance of the restored workspace (`rebuild`, `reindex_file`, `add_readonly_roots`, agent and
lock bookkeeping) runs in the prefix, and reads of command handles and their output are served from the
recording. A rebuild of a root outside the restored workspace, the source-writing refactors, and host leases
are still refused.
