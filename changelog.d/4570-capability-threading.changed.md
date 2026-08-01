- **`harn fix` gives a helper the narrowest capability it actually uses.** When
  the fixer adds a capability parameter to a function that had none, it now
  picks the handle that covers the effects in that function's body rather than
  the root `Harness`. A helper that only reads files becomes
  `fn helper(harness: HarnessFs)` called as `helper(harness.fs)`, and one that
  spans two capabilities takes a record such as
  `{fs: HarnessFs, system: HarnessSystem}`. Signatures state their authority on
  the first pass, so the migration no longer lands a root grant that a later
  attenuation pass has to walk back.
