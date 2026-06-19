- **Running a compiled chunk no longer deep-copies its bytecode.** `Vm::execute`
  used to clone the entire `Chunk` (bytecode + constant pool + side tables) on
  every top-level run. The internal run path now threads the shared
  `Arc<Chunk>` straight into the call frame, and a new `Vm::execute_arc(ChunkRef)`
  entry point lets callers that re-run the same chunk (the `harn serve` request
  path, ACP, record filters) pay a refcount bump instead of an `O(code)` copy.
  `Vm::execute(&Chunk)` is unchanged for one-shot callers.
