- Strip `*_PASSWORD`, `*_PASSWD`, and `*_CREDENTIALS` parent environment
  variables from `run`/`command_run` child processes, so secrets like
  `DOCKER_PASSWORD` no longer leak into the tool-result stdout the model sees.
