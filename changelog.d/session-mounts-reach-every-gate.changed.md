A root mounted on the active session with `agent_session_add_root` now reaches every gate on the
next call: the workspace boundary admits it by mode, the file builtins and the confined child
profile read a `read_only` mount, and an `extend` mount is writable. Before, a mount was visible
only to the path-scope hook, so the boundary and the child sandbox still refused it.
