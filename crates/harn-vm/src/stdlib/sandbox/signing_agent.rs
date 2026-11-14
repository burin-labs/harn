//! An ssh-agent socket that only signs, with one key, for one SSHSIG
//! namespace.
//!
//! Granting a confined process the person's own agent socket lets it list
//! every loaded key, remove or add keys, lock the agent, and sign SSH
//! authentication challenges for any host its network policy admits. A host
//! that only wants signatures over files (for example, `git commit -S` with
//! `gpg.format=ssh`, which signs with namespace `git`) serves this filter
//! instead and grants its socket as the process sandbox's SSH agent socket.
//!
//! The filter answers `REQUEST_IDENTITIES` with the configured key alone, when
//! the upstream agent holds it, and forwards a `SIGN_REQUEST` only when it
//! names that key and its data is an SSHSIG payload for the configured
//! namespace. An authentication signature never has that shape: its data
//! starts with the session identifier, not the `SSHSIG` magic. Every other
//! request gets `SSH_AGENT_FAILURE` without reaching the upstream agent.
//!
//! This narrows what the grant can do; it does not narrow what it signs. The
//! filter cannot see the message, only its hash, so anything that can connect
//! to the socket can obtain the person's signature over any hash in the
//! namespace: for `git`, a valid signature on any commit or tag it chooses,
//! for as long as the filter serves. Grant it only where that is acceptable.
//!
//! Each client is served on its own thread, up to [`MAX_CLIENTS`] at once, and
//! must finish within [`CONNECTION_DEADLINE`], so a client that stalls, trickles
//! bytes, or never reads its replies holds only its own connection. Dropping
//! the filter shuts every open connection down and returns without waiting
//! for them.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine as _;

const FAILURE: u8 = 5;
const REQUEST_IDENTITIES: u8 = 11;
const IDENTITIES_ANSWER: u8 = 12;
const SIGN_REQUEST: u8 = 13;
const SIGN_RESPONSE: u8 = 14;
/// Sign-request flags the filter forwards: none, or an RSA SHA-2 hash choice.
const ALLOWED_FLAGS: [u32; 3] = [0, 2, 4];

/// OpenSSH's own limit on one agent message.
const MAX_MESSAGE: usize = 256 * 1024;
/// How long one read or write may wait on the upstream agent.
const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(10);
/// The whole life of one client connection. A signing client connects, lists,
/// signs, and closes in well under this.
pub const CONNECTION_DEADLINE: Duration = Duration::from_secs(30);
/// Connections served at once; more are closed unanswered.
pub const MAX_CLIENTS: usize = 8;
/// How often the accept loop checks for a stop when no client is connecting.
const ACCEPT_POLL: Duration = Duration::from_millis(50);
/// The socket's name inside the directory the host gives [`SigningAgent::serve`].
pub const SOCKET: &str = "signing-agent.sock";

/// What the filter signs for.
#[derive(Clone, Debug)]
pub struct SigningScope {
    /// The public key, as an `authorized_keys` line or `key::` value
    /// (`ssh-ed25519 AAAA... comment`). The comment is ignored.
    pub public_key: String,
    /// The SSHSIG namespace every signature must carry, such as `git`.
    pub namespace: String,
}

/// Open client connections, kept so a stop can shut each one down.
#[derive(Default)]
struct Clients {
    next: u64,
    open: HashMap<u64, UnixStream>,
}

/// Serves the filter until dropped, then removes its socket.
pub struct SigningAgent {
    path: PathBuf,
    stop: Arc<AtomicBool>,
    clients: Arc<Mutex<Clients>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl SigningAgent {
    /// Bind [`SOCKET`] in `dir`, which must be a directory owned by this user
    /// that no one else can open (mode `0700`), and forward permitted requests
    /// to the agent at `upstream`. A socket left there by a filter that is no
    /// longer serving is replaced; a live one is refused.
    pub fn serve(dir: &Path, upstream: &Path, scope: SigningScope) -> std::io::Result<Self> {
        let key = key_blob(&scope.public_key).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "the signing key is not an SSH public key",
            )
        })?;
        let policy = Arc::new(Policy {
            key,
            namespace: scope.namespace.into_bytes(),
            upstream: upstream.to_path_buf(),
        });
        check_private_dir(dir)?;
        let path = dir.join(SOCKET);
        clear_stale_socket(&path)?;
        let listener = UnixListener::bind(&path)?;
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let clients = Arc::new(Mutex::new(Clients::default()));
        let (stopping, open) = (Arc::clone(&stop), Arc::clone(&clients));
        let thread = crate::task_spawn::thread(move || {
            while !stopping.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => admit(&policy, &open, &stopping, stream),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(ACCEPT_POLL);
                    }
                    Err(_) => std::thread::sleep(ACCEPT_POLL),
                }
            }
        });
        Ok(Self {
            path,
            stop,
            clients,
            thread: Some(thread),
        })
    }

    /// The socket to grant as `SSH_AUTH_SOCK`.
    pub fn socket(&self) -> &Path {
        &self.path
    }
}

impl Drop for SigningAgent {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let open = std::mem::take(
            &mut self
                .clients
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .open,
        );
        for stream in open.values() {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
        // The accept loop polls the stop, so this returns within one poll.
        // Client threads are not joined: their connections are shut down, and
        // one still waiting on the upstream agent ends at its own timeout.
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Serve `stream` on its own thread, or close it when the filter is full or
/// stopping.
fn admit(
    policy: &Arc<Policy>,
    clients: &Arc<Mutex<Clients>>,
    stopping: &Arc<AtomicBool>,
    stream: UnixStream,
) {
    let id = {
        let mut clients = clients.lock().unwrap_or_else(|poison| poison.into_inner());
        if clients.open.len() >= MAX_CLIENTS || stopping.load(Ordering::SeqCst) {
            return;
        }
        let Ok(handle) = stream.try_clone() else {
            return;
        };
        let id = clients.next;
        clients.next += 1;
        clients.open.insert(id, handle);
        id
    };
    let (policy, clients) = (Arc::clone(policy), Arc::clone(clients));
    crate::task_spawn::thread(move || {
        let _ = policy.serve_client(stream, Instant::now() + CONNECTION_DEADLINE);
        clients
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .open
            .remove(&id);
    });
}

/// Refuse a socket directory another user owns or can open.
fn check_private_dir(dir: &Path) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(dir)?;
    // SAFETY: geteuid has no arguments or preconditions.
    let uid = unsafe { libc::geteuid() };
    if !meta.is_dir() || meta.uid() != uid || meta.mode() & 0o077 != 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "the signing agent's directory must be a 0700 directory this user owns",
        ));
    }
    Ok(())
}

/// Remove a socket at `path` that nothing serves. A live socket, or anything
/// that is not a socket, is refused.
fn clear_stale_socket(path: &Path) -> std::io::Result<()> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if !meta.file_type().is_socket() || UnixStream::connect(path).is_ok() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            "the signing agent's socket path is already in use",
        ));
    }
    std::fs::remove_file(path)
}

struct Policy {
    key: Vec<u8>,
    namespace: Vec<u8>,
    upstream: PathBuf,
}

impl Policy {
    fn serve_client(&self, mut client: UnixStream, deadline: Instant) -> std::io::Result<()> {
        // Accepted from a nonblocking listener; some platforms pass that on.
        client.set_nonblocking(false)?;
        while let Some(request) = read_message(&mut client, deadline)? {
            let reply = self.answer(&request).unwrap_or_else(|| vec![FAILURE]);
            write_message(&mut client, &reply, deadline)?;
        }
        Ok(())
    }

    /// The reply body for one request, or `None` for a failure.
    fn answer(&self, request: &[u8]) -> Option<Vec<u8>> {
        match *request.first()? {
            REQUEST_IDENTITIES => self.identities(),
            SIGN_REQUEST if self.permits(&request[1..]) => {
                let reply = self.forward(request)?;
                (reply.first() == Some(&SIGN_RESPONSE)).then_some(reply)
            }
            _ => None,
        }
    }

    /// The configured key alone, and only when the upstream agent lists it.
    /// Upstream gets a canonical request, whatever bytes the client sent.
    fn identities(&self) -> Option<Vec<u8>> {
        let reply = self.forward(&[REQUEST_IDENTITIES])?;
        let mut body = Reader::new(reply.get(1..)?);
        if reply.first() != Some(&IDENTITIES_ANSWER) {
            return None;
        }
        let count = body.u32()?;
        let mut held = None;
        for _ in 0..count {
            let blob = body.string()?;
            let comment = body.string()?;
            if blob == self.key.as_slice() {
                held = Some(comment.to_vec());
            }
        }
        let mut answer = vec![IDENTITIES_ANSWER];
        match held {
            Some(comment) => {
                answer.extend(1u32.to_be_bytes());
                put_string(&mut answer, &self.key);
                put_string(&mut answer, &comment);
            }
            None => answer.extend(0u32.to_be_bytes()),
        }
        Some(answer)
    }

    /// Whether a sign request names the configured key and carries an SSHSIG
    /// payload for the configured namespace, with nothing after it, and no
    /// flag beyond an RSA SHA-2 hash choice.
    fn permits(&self, body: &[u8]) -> bool {
        let mut request = Reader::new(body);
        let (Some(key), Some(data), Some(flags)) =
            (request.string(), request.string(), request.u32())
        else {
            return false;
        };
        if !request.is_empty() || key != self.key.as_slice() || !ALLOWED_FLAGS.contains(&flags) {
            return false;
        }
        let Some(payload) = data.strip_prefix(b"SSHSIG") else {
            return false;
        };
        let mut payload = Reader::new(payload);
        let (Some(namespace), Some(_reserved), Some(algorithm), Some(hash)) = (
            payload.string(),
            payload.string(),
            payload.string(),
            payload.string(),
        ) else {
            return false;
        };
        let hash_len = match algorithm {
            b"sha256" => 32,
            b"sha512" => 64,
            _ => return false,
        };
        payload.is_empty() && namespace == self.namespace.as_slice() && hash.len() == hash_len
    }

    fn forward(&self, request: &[u8]) -> Option<Vec<u8>> {
        let mut upstream = UnixStream::connect(&self.upstream).ok()?;
        let deadline = Instant::now() + UPSTREAM_TIMEOUT;
        write_message(&mut upstream, request, deadline).ok()?;
        read_message(&mut upstream, deadline).ok()?
    }
}

/// The wire blob of a public key line's base64 field.
fn key_blob(public_key: &str) -> Option<Vec<u8>> {
    let line = public_key.trim();
    let line = line.strip_prefix("key::").unwrap_or(line);
    let mut fields = line.split_whitespace();
    let algorithm = fields.next()?;
    let blob = base64::engine::general_purpose::STANDARD
        .decode(fields.next()?)
        .ok()?;
    // The blob begins with its own algorithm name; a mismatch is not a key.
    (Reader::new(&blob).string()? == algorithm.as_bytes()).then_some(blob)
}

/// One length-prefixed message, or `None` at a clean end of stream. Fails
/// once `deadline` passes, however the bytes arrive.
fn read_message(stream: &mut UnixStream, deadline: Instant) -> std::io::Result<Option<Vec<u8>>> {
    let mut length = [0u8; 4];
    match read_exact_by(stream, &mut length, deadline) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error),
    }
    let length = u32::from_be_bytes(length) as usize;
    if length == 0 || length > MAX_MESSAGE {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "agent message length out of range",
        ));
    }
    let mut body = vec![0u8; length];
    read_exact_by(stream, &mut body, deadline)?;
    Ok(Some(body))
}

/// Write one length-prefixed message, failing once `deadline` passes.
fn write_message(stream: &mut UnixStream, body: &[u8], deadline: Instant) -> std::io::Result<()> {
    let length = u32::try_from(body.len())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "message too long"))?;
    let mut message = Vec::with_capacity(body.len() + 4);
    message.extend(length.to_be_bytes());
    message.extend(body);
    let mut written = 0;
    while written < message.len() {
        stream.set_write_timeout(Some(time_left(deadline)?))?;
        match stream.write(&message[written..]) {
            Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
            Ok(count) => written += count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn read_exact_by(
    stream: &mut UnixStream,
    buf: &mut [u8],
    deadline: Instant,
) -> std::io::Result<()> {
    let mut filled = 0;
    while filled < buf.len() {
        stream.set_read_timeout(Some(time_left(deadline)?))?;
        match stream.read(&mut buf[filled..]) {
            Ok(0) => return Err(std::io::ErrorKind::UnexpectedEof.into()),
            Ok(count) => filled += count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// The time before `deadline`, or a timeout error once it has passed.
fn time_left(deadline: Instant) -> std::io::Result<Duration> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err(std::io::ErrorKind::TimedOut.into());
    }
    Ok(left)
}

fn put_string(out: &mut Vec<u8>, value: &[u8]) {
    out.extend((value.len() as u32).to_be_bytes());
    out.extend(value);
}

/// Reads SSH wire `uint32` and `string` fields.
struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self(bytes)
    }

    fn u32(&mut self) -> Option<u32> {
        let (head, rest) = self.0.split_first_chunk::<4>()?;
        self.0 = rest;
        Some(u32::from_be_bytes(*head))
    }

    fn string(&mut self) -> Option<&'a [u8]> {
        let length = self.u32()? as usize;
        if length > self.0.len() {
            return None;
        }
        let (value, rest) = self.0.split_at(length);
        self.0 = rest;
        Some(value)
    }

    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[cfg(all(test, unix))]
#[path = "signing_agent_tests.rs"]
mod tests;
