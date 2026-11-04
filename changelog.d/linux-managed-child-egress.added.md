Child-process egress through the managed proxy now works on Linux when the policy names the namespace launcher.
The child runs in a private network namespace whose only way out is the proxy, so raw sockets, datagrams, and
direct DNS have no route.
