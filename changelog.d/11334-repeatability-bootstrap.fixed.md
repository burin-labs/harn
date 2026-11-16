Touched-test selection uses a released bootstrap CLI before Rust cache setup,
avoiding a source build just to select tests. The inventory reader and tests
continue to use the checked-out source.
