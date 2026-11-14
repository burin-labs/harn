//! The login a granted git uses, served over a per-spawn socket instead of
//! stored in a file.
//!
//! Git's built-in `credential-cache` helper asks a daemon on a Unix socket for
//! a login. Harn answers that protocol itself, from memory, for the one spawn.
//! A socket cannot be opened and read as a file, so a repository that points
//! `shallow`, an alternate, a ref, or any other path git reads at the socket
//! (or at anything else in the per-spawn directory) reads no login. Only the
//! granted git's own sandbox profile may connect to the socket.
//!
//! The socket answers `get` only for the granted origin and the repository
//! paths the command reaches, and ignores every other action.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// The socket's name in the per-spawn directory.
pub(crate) const SOCKET: &str = "credential.sock";

/// A client that sends nothing is dropped after this long, so one stuck
/// connection cannot stop the socket from answering the next.
const CLIENT_TIMEOUT: Duration = Duration::from_secs(10);

/// What the socket answers: a login for these repository paths at one origin.
pub(crate) struct Login {
    /// `https` (or `http` for a test server).
    pub(crate) protocol: String,
    /// The host as git sends it, with a port when the URL has one.
    pub(crate) host: String,
    pub(crate) paths: Vec<String>,
    pub(crate) username: String,
    pub(crate) password: String,
}

/// Serves one spawn's login until dropped.
pub(crate) struct CredentialSocket {
    path: PathBuf,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl CredentialSocket {
    pub(crate) fn serve(dir: &Path, login: Login) -> std::io::Result<Self> {
        let path = dir.join(SOCKET);
        let listener = UnixListener::bind(&path)?;
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        let thread = crate::task_spawn::thread(move || {
            for stream in listener.incoming() {
                if stopping.load(Ordering::SeqCst) {
                    break;
                }
                if let Ok(stream) = stream {
                    let _ = answer(stream, &login);
                }
            }
        });
        Ok(Self {
            path,
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for CredentialSocket {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the accept loop so it sees the stop.
        let _ = UnixStream::connect(&self.path);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Read one `credential-cache` request (`key=value` lines until the client
/// shuts down its side) and answer a matching `get` with the login.
fn answer(stream: UnixStream, login: &Login) -> std::io::Result<()> {
    stream.set_read_timeout(Some(CLIENT_TIMEOUT))?;
    let mut fields = std::collections::BTreeMap::new();
    for line in BufReader::new(&stream).lines() {
        let line = line?;
        if let Some((key, value)) = line.split_once('=') {
            fields
                .entry(key.to_string())
                .or_insert_with(|| value.to_string());
        }
    }
    let field = |key: &str| fields.get(key).map(String::as_str);
    let matches = field("action") == Some("get")
        && field("protocol") == Some(login.protocol.as_str())
        && field("host") == Some(login.host.as_str())
        && field("path").is_some_and(|path| login.paths.iter().any(|granted| granted == path));
    if matches {
        (&stream).write_all(
            format!("username={}\npassword={}\n", login.username, login.password).as_bytes(),
        )?;
    }
    Ok(())
}
