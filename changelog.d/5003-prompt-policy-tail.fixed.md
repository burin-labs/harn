Move the prompt-text ownership check out of the Rust compile job and run it with CI's shared Harn binary, removing a
four-minute merge-queue tail without reducing coverage.
