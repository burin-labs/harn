//! The host's end of a namespaced child's proxy connections: a Unix socket for
//! each proxy port. See [`crate::process_sandbox::EgressRelayTarget`].

use std::fs::{self, DirBuilder, File};
use std::io;
use std::net::TcpStream;
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use crate::orchestration::ProcessNetworkProxy;
use crate::process_sandbox::egress_relay::{
    socket_through, splice, EgressRelayTarget, HTTP_SOCKET, SOCKS_SOCKET,
};

#[derive(Debug)]
pub(super) struct NamespaceBridge {
    directory: PathBuf,
    held: File,
    shutdown: Arc<AtomicBool>,
    joins: Vec<JoinHandle<()>>,
}

impl NamespaceBridge {
    /// Listen for this proxy. Creating the directory fails if the name is taken,
    /// so no other account can have placed a socket here first.
    pub(super) fn start(proxy: ProcessNetworkProxy) -> Result<Self, String> {
        let describe = |error: io::Error| format!("bridge child egress proxy: {error}");
        let directory = EgressRelayTarget::for_proxy(proxy).directory;
        DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .map_err(describe)?;
        let held = File::open(&directory).map_err(describe)?;
        let shutdown = Arc::new(AtomicBool::new(false));
        let mut joins = Vec::with_capacity(2);
        for (socket, port) in [
            (HTTP_SOCKET, proxy.http_port),
            (SOCKS_SOCKET, proxy.socks_port),
        ] {
            let listener = UnixListener::bind(socket_through(&held, socket)).map_err(describe)?;
            joins.push(accept_loop(listener, port, Arc::clone(&shutdown)).map_err(describe)?);
        }
        Ok(Self {
            directory,
            held,
            shutdown,
            joins,
        })
    }
}

impl Drop for NamespaceBridge {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        for socket in [HTTP_SOCKET, SOCKS_SOCKET] {
            let _ = UnixStream::connect(socket_through(&self.held, socket));
        }
        for join in self.joins.drain(..) {
            let _ = join.join();
        }
        let _ = fs::remove_dir_all(&self.directory);
    }
}

// Shared proxy infrastructure: it serves every run, so its threads carry no run's scope.
#[allow(clippy::disallowed_methods)]
fn accept_loop(
    listener: UnixListener,
    port: u16,
    shutdown: Arc<AtomicBool>,
) -> io::Result<JoinHandle<()>> {
    crate::runtime_stack::builder()
        .name("harn-process-egress-bridge".to_string())
        .spawn(move || {
            while let Ok((client, _peer)) = listener.accept() {
                if shutdown.load(Ordering::Acquire) {
                    break;
                }
                let _ = crate::runtime_stack::builder()
                    .name("harn-process-egress-bridge-connection".to_string())
                    .spawn(move || {
                        // The proxy's own connection limit and timeouts apply on
                        // the far side of this one.
                        let proxy = TcpStream::connect(("127.0.0.1", port))?;
                        splice(client, proxy)
                    });
            }
        })
}
