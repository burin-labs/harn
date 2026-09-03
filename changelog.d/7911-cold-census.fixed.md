The stack-frame budget is measured from a build of its own. Clippy replays
cached diagnostics, so a census taken in a warm target directory reported
whatever threshold the previous build used and silently under-measured every
crate it did not rebuild. The banked baseline was therefore too low for most of
the workspace, and continuous integration, which builds cold, refused pull
requests that had changed nothing it measures.
