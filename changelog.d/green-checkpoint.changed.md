Releases are now cut from the protected `green` branch, the newest `main` commit
whose full CI suite passed. Promotion publishes only a commit `green` contains,
and the scheduled consumer canary rehearses `green` instead of `main`'s head.
