- Removed three dead codepaths: the unused `std/collections` helpers `store_stale` / `store_refresh` (zero call
  sites), and the dead `"adapter_shim"` `callsite_strategy` branch in `std/edit`'s `add_parameter` (it only ever
  returned "not yet supported"). Internal: `harn-cli`'s bundle signer now uses `hex::encode` instead of a duplicate
  local `hex_encode` helper.
