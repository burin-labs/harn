`ApiServerConfig::with_host_responder` now takes an
`Arc<dyn ApiHostResponderBinder>` instead of a single responder, so each
session is answered for its own workspace root.

Migration: implement `HostResponderBinder::bind(&self, session_root)` to return
the `Arc<dyn HostResponder>` for that root (cache by root if building one is
expensive) and pass the binder where the responder was passed.
