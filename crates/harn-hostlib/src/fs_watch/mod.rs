//! File-system watch host capability.
//!
//! Wraps `notify` to deliver change events to subscribers identified by
//! handle. The public contract is registered while the implementation is
//! still pending.

use crate::registry::{BuiltinRegistry, HostlibCapability};

/// File-watch capability handle.
#[derive(Default)]
pub struct FsWatchCapability;

impl HostlibCapability for FsWatchCapability {
    fn module_name(&self) -> &'static str {
        "fs_watch"
    }

    fn register_builtins(&self, registry: &mut BuiltinRegistry) {
        registry.register_unimplemented("hostlib_fs_watch_subscribe", "fs_watch", "subscribe");
        registry.register_unimplemented("hostlib_fs_watch_unsubscribe", "fs_watch", "unsubscribe");
    }
}
