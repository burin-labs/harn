- **User-function calls allocate less on the call hot path.** Entering a
  closure frame previously performed two `VmEnv` (scope-stack) heap clones — one
  to snapshot the caller's env for restore-on-return, and one to build the
  callee's env — plus a reallocation when the freshly cloned callee env was
  grown by the empty scope every call pushes. The caller-env snapshot is now a
  move (`std::mem::replace`) rather than a clone, since `self.env` is overwritten
  with the callee env anyway and the old value only needs to survive in the
  frame; and the callee-env clone now reserves room for that pushed scope so it
  no longer reallocates. A measured user-function call drops from 5 heap
  allocations / 81 bytes to 3 / 41 bytes, with identical scoping, recursion, and
  closure-capture semantics. The change is confined to runtime env handling (no
  bytecode, compiler, or serialized-cache change) and touches only owned local
  state, so it is safe under the multi-threaded runtime. No `harn` language
  behavior changes.
