An embedder can serve `harn_vm::stdlib::sandbox::signing_agent::SigningAgent` in front of
the user's ssh-agent and grant its socket as the process sandbox's SSH agent.
It lists one configured key and signs only SSHSIG payloads for one namespace
with it, so a confined child can sign git objects without reaching the
agent's other keys, SSH authentication, or key management.
Each client is served on its own bounded thread with a total deadline, so a
stalled client cannot block another or hold teardown. The socket directory
must be private to the user.
