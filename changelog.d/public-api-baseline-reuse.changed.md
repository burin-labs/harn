Public API checks reuse documentation from the exact baseline commit when trusted main CI produced it with the same
compiler and flags. Missing or invalid documentation falls back to rebuilding the baseline. The check also refuses an
incomplete crate census when a comparator fails before reporting its result.
