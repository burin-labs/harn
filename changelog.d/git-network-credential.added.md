A `for_command: "git"` grant gives `git fetch`, `git push` and `git ls-remote`
an HTTPS login for github.com on macOS. The granted git runs from a private
git directory with Harn-written config, so no repository hook, helper, alias,
proxy or other setting runs or sees the login.
