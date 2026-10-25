ACP `module_preparation` progress now reports `completed` when the pipeline starts running, not when the
turn ends, so clients stop showing "Preparing modules" for the whole turn. Later child-VM module loads no
longer reopen it.
