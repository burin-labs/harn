//! Deterministic local HTTP fixtures shared by egress tests.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread::{self, JoinHandle};

pub(crate) struct OneShotHttpServer {
    port: u16,
    handle: Option<JoinHandle<()>>,
}

impl OneShotHttpServer {
    pub(crate) fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback probe server");
        let port = listener.local_addr().expect("probe server addr").port();
        let handle = thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                );
                let _ = stream.flush();
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

    pub(crate) fn join(mut self) {
        self.handle
            .take()
            .expect("probe server handle")
            .join()
            .expect("probe server thread");
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
