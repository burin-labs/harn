Pull requests that require downstream consumer verification now observe the original rehearsal across credential windows
through its declared queue and execution budget. An attempt that reaches its full deadline without a verdict blocks the
pull request.
