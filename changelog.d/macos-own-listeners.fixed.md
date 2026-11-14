- On macOS, a confined command with a loopback grant behind the managed network proxy can connect to local servers it
  started again, so test suites that serve and call themselves, and dev servers reached with `curl localhost`, work.
  Each spawn denies the ports that other local services listen on, and `NO_PROXY` names the loopback addresses.
