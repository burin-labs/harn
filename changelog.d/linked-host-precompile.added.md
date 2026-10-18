- **A linked host can precompile adjacent bytecode with its own VM.**
  `harn_package::precompile::precompile_tree(root)` and `precompile_source` write
  the same entry, module and interface artifacts as `harn precompile`, keyed by
  the calling binary's builtin contract. A host whose `harn-vm` families differ
  from the CLI's cannot load the CLI's artifacts; calling this from the host's
  own binary produces artifacts it loads. `harn precompile` now projects the same
  function.
