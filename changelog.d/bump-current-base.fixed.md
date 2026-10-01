Runtime bumps can refresh an armed pull request after main advances. The updater guards the current main branch head
when disarming auto-merge, rather than the historical base recorded on the pull request, and refuses the operation
when the current head cannot be read.
