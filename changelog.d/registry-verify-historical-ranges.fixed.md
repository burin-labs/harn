`harn package registry verify --remote` no longer fails a published version whose
`harn` range excludes the verifying runtime. It still rejects a malformed range,
and install keeps refusing incompatible packages.
