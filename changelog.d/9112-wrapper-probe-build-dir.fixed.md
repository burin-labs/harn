Compiler-cache wrapper probes keep Cargo intermediates inside their private scratch
directory, including when the caller configures a separate Cargo build directory.
