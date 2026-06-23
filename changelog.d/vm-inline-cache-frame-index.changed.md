- **VM inline-cache dispatch now avoids per-op hash lookups.** Call frames cache
  the VM-local inline-cache set for their chunk once at entry, so adaptive
  binary, property, method, and direct-call cache reads/writes index directly
  into VM-local feedback during hot dispatch.
