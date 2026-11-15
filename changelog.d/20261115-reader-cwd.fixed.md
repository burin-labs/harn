Resolve command-reader paths from explicit working directories and literal POSIX
`cd` success chains before workspace approval. Keep workspace authority fixed and
retain conservative checks for ambiguous shell control flow or earlier processes
that could change later directory entries.
