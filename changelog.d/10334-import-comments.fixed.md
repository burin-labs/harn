Harn formatting keeps module comments above sorted imports. The formatter's corpus check
now accepts its documented import ordering while still rejecting changed imports,
missing comments, and changed program tokens.

`harn fmt` also accepts explicit `.harn.txt` program files, using the same
file rules as directory formatting and source repair.
