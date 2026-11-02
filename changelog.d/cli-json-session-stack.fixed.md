Keep JSON run terminal state on the heap so nested CLI execution futures do not multiply its stack cost.
Managed commands still retire before the terminal event.
