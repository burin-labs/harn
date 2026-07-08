The coding-agent eval harness and stdlib file writers now avoid redundant `mkdir` calls on an
already-mounted sandbox workspace root, restoring mock-matrix release audit coverage under strict
workspace-root write enforcement.
