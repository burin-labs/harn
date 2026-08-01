Typed Harness and builtin hot paths now keep a VM-local receipt-equivalent
effect-call memo beside the shared execution-tree recorder. Exact contract and
resource-bearing argument matches skip the shared mutex, while collisions and
evictions only cost work and never drop evidence.
