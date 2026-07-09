Forward `env_remove` through stdlib command helpers, agent command tools, and
command-policy rewrites so host integrations can strip caller-selected child
environment variables without replacing the rest of the inherited environment.
