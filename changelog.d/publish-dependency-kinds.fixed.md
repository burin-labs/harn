Crate publication now orders normal and build dependencies without treating
test-only dependency cycles as upload prerequisites. The dry run checks the
same publication graph before packaging the workspace.
