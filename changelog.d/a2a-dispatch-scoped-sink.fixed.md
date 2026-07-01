A2A streaming tasks now attach their worker-event sink directly to the active
dispatch, so progress status updates keep streaming even if the process-global
agent-event registry is reset by sibling work.
