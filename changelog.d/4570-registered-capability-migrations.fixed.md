`harn lint` and `harn fix` now know where every runtime-registered capability
method went. Store, checkpoint, metadata, and other methods installed on the VM
at startup never reach the builtin manifest, so calls to their pre-cutover
global names reported only "not defined" with no repair. They now report
`HARN-LNT-071` naming the owning capability, and `harn fix --apply --safety
surface-changing` rewrites them and threads the handle.

`secret_get`, which the connector runtime used to inject for the duration of an
export call, migrates to `harness.secrets.read`.
