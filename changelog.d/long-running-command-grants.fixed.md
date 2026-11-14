A command run through the long-running path, which includes any command with
an auto-background window, now receives its command-bound grant, the same as
a plain foreground command. Before, a granted program run that way got none.
Its output is redacted as it is read, so progress snapshots, output waits, and
the output files carry no granted value, even one split across reads.
