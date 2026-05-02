mod acp_hub;
mod admin;
mod core;
mod routes;

pub(crate) use admin::{AdminReloadHandle, AdminReloadRequest};
pub(crate) use core::{ListenerConfig, ListenerRuntime};
#[allow(unused_imports)]
pub(crate) use routes::{
    AuthMode, ListenerAuth, RouteConfig, SignatureMode, TriggerMetricSnapshot,
};

#[cfg(test)]
// Tests hold the shared `lock_harn_state` guard across `.await` points; the
// guard is dropped when each `#[tokio::test]` future resolves.
#[allow(clippy::await_holding_lock)]
mod tests;
