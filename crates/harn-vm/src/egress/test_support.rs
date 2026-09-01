//! Egress test scaffolding: the policy/env guards every egress-touching test
//! installs, plus the deterministic local HTTP fixtures they drive.

use super::*;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread::{self, JoinHandle};

pub(crate) struct OneShotHttpServer {
    port: u16,
    handle: Option<JoinHandle<bool>>,
}

impl OneShotHttpServer {
    pub(crate) fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback probe server");
        let port = listener.local_addr().expect("probe server addr").port();
        let handle = thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                // A real client sends request bytes; the release connect from
                // `unblock_and_join`/`Drop` sends none. The distinction lets
                // tests assert a request genuinely arrived.
                let served = matches!(stream.read(&mut buf), Ok(read) if read > 0);
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                );
                let _ = stream.flush();
                served
            } else {
                false
            }
        });
        Self {
            port,
            handle: Some(handle),
        }
    }

    pub(crate) fn url(&self) -> String {
        format!("http://localhost:{}/probe", self.port)
    }

    pub(crate) fn port(&self) -> u16 {
        self.port
    }

    /// Joins the server thread, returning whether a nonempty HTTP request
    /// reached it.
    pub(crate) fn join(mut self) -> bool {
        self.handle
            .take()
            .expect("probe server handle")
            .join()
            .expect("probe server thread")
    }

    /// Releases a server whose client request was correctly blocked before the
    /// TCP connect. This proves the guard blocked a reachable listener without
    /// leaking an accept thread.
    pub(crate) fn unblock_and_join(self) {
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        self.join();
    }
}

impl Drop for OneShotHttpServer {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = TcpStream::connect(("127.0.0.1", self.port));
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
pub fn reset_egress_policy_for_tests() {
    reset_egress_policy_for_host();
}

/// Gives a test a clean egress universe with hermetic edges: it layers this
/// domain's policy reset over the shared env seam
/// ([`crate::test_env::test_env_guard`]). On both creation and drop the inner
/// guard clears this thread's env overrides while this wrapper resets this
/// thread's egress policy state, so neither ambient configuration nor a sibling
/// test's leftovers can leak in, and nothing leaks out. All `cfg(test)` egress
/// state is thread-keyed, so no cross-test serialization is needed and the
/// guard is safe to hold across `await` points.
///
/// This governs only the *inputs* to policy installation;
/// `harness.net.egress_policy(...)`'s deliberate refuse-to-override behavior is
/// unchanged.
#[cfg(test)]
#[must_use]
pub(crate) fn test_env_guard() -> EgressTestEnvGuard {
    // The inner guard clears the shared env overrides on creation; layer the
    // egress-specific policy reset on top.
    let inner = crate::test_env::test_env_guard();
    reset_egress_policy_for_host();
    EgressTestEnvGuard { inner }
}

/// Guard returned by [`test_env_guard`]. Injects `HARN_EGRESS_*` values for
/// this thread via [`EgressTestEnvGuard::set`] and, on drop, resets this
/// thread's egress policy state on top of the inner guard clearing the shared
/// env overrides.
#[cfg(test)]
pub(crate) struct EgressTestEnvGuard {
    inner: crate::test_env::TestEnvGuard,
}

#[cfg(test)]
impl EgressTestEnvGuard {
    /// Sets a `HARN_EGRESS_*` variable for this thread only, visible to the
    /// shared env seam ([`crate::test_env::env_var_seamed`]) readers on the
    /// same thread.
    pub(crate) fn set(&self, key: &str, value: &str) {
        self.inner.set(key, value);
    }
}

#[cfg(test)]
impl Drop for EgressTestEnvGuard {
    fn drop(&mut self) {
        // Reset the egress policy state; the `inner` field's Drop then clears
        // the shared env overrides. Neither read depends on the other's order.
        reset_egress_policy_for_host();
    }
}

/// A clean egress configuration scope for constructing a test client.
#[cfg(test)]
pub(crate) struct EgressTestConfigGuard {
    _env: EgressTestEnvGuard,
}

#[cfg(test)]
impl EgressTestConfigGuard {
    pub(crate) fn new() -> Self {
        Self {
            _env: test_env_guard(),
        }
    }
}

/// Install a thread-local egress policy from `(key, value)` config pairs for
/// tests that need to drive the real HTTP client path without touching
/// process-global `HARN_EGRESS_*` env (which is unsound under concurrency).
#[cfg(test)]
pub(crate) fn install_test_policy(config: &[(&str, VmValue)]) {
    let map = config
        .iter()
        .cloned()
        .map(|(key, value)| (key.to_string(), value))
        .collect();
    let (policy, declared) = policy_from_config(&map).expect("test egress policy parses");
    install_policy(policy, declared, "test").expect("test egress policy installs");
}
