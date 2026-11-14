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

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;

const FAILURE: u8 = 5;
const REQUEST_IDENTITIES: u8 = 11;
const IDENTITIES_ANSWER: u8 = 12;
const SIGN_REQUEST: u8 = 13;
const SIGN_RESPONSE: u8 = 14;

/// OpenSSH's own limit on one agent message.
const MAX_MESSAGE: usize = 256 * 1024;
/// A client that stops mid-message is dropped after this long, so one stuck
/// connection cannot keep the filter from answering the next.
const CLIENT_TIMEOUT: Duration = Duration::from_secs(10);
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

/// Serves the filter until dropped, then removes its socket.
pub struct SigningAgent {
    path: PathBuf,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl SigningAgent {
    /// Bind [`SOCKET`] in `dir`, which the host creates private to the user,
    /// and forward permitted requests to the agent at `upstream`.
    pub fn serve(dir: &Path, upstream: &Path, scope: SigningScope) -> std::io::Result<Self> {
        let key = key_blob(&scope.public_key).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "the signing key is not an SSH public key",
            )
        })?;
        let policy = Policy {
            key,
            namespace: scope.namespace.into_bytes(),
            upstream: upstream.to_path_buf(),
        };
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
                    let _ = policy.serve_client(stream);
                }
            }
        });
        Ok(Self {
            path,
            stop,
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
        // Wake the accept loop so it sees the stop.
        let _ = UnixStream::connect(&self.path);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

struct Policy {
    key: Vec<u8>,
    namespace: Vec<u8>,
    upstream: PathBuf,
}

impl Policy {
    fn serve_client(&self, mut client: UnixStream) -> std::io::Result<()> {
        client.set_read_timeout(Some(CLIENT_TIMEOUT))?;
        while let Some(request) = read_message(&mut client)? {
            let reply = self.answer(&request).unwrap_or_else(|| vec![FAILURE]);
            write_message(&mut client, &reply)?;
        }
        Ok(())
    }

    /// The reply body for one request, or `None` for a failure.
    fn answer(&self, request: &[u8]) -> Option<Vec<u8>> {
        match *request.first()? {
            REQUEST_IDENTITIES => self.identities(request),
            SIGN_REQUEST if self.permits(&request[1..]) => {
                let reply = self.forward(request)?;
                (reply.first() == Some(&SIGN_RESPONSE)).then_some(reply)
            }
            _ => None,
        }
    }

    /// The configured key alone, and only when the upstream agent lists it.
    fn identities(&self, request: &[u8]) -> Option<Vec<u8>> {
        let reply = self.forward(request)?;
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
    /// payload for the configured namespace, with nothing after it.
    fn permits(&self, body: &[u8]) -> bool {
        let mut request = Reader::new(body);
        let (Some(key), Some(data), Some(_flags)) =
            (request.string(), request.string(), request.u32())
        else {
            return false;
        };
        if !request.is_empty() || key != self.key.as_slice() {
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
        upstream.set_read_timeout(Some(CLIENT_TIMEOUT)).ok()?;
        write_message(&mut upstream, request).ok()?;
        read_message(&mut upstream).ok()?
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

/// One length-prefixed message, or `None` at a clean end of stream.
fn read_message(stream: &mut UnixStream) -> std::io::Result<Option<Vec<u8>>> {
    let mut length = [0u8; 4];
    match stream.read_exact(&mut length) {
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
    stream.read_exact(&mut body)?;
    Ok(Some(body))
}

fn write_message(stream: &mut UnixStream, body: &[u8]) -> std::io::Result<()> {
    let length = u32::try_from(body.len())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "message too long"))?;
    let mut message = Vec::with_capacity(body.len() + 4);
    message.extend(length.to_be_bytes());
    message.extend(body);
    stream.write_all(&message)
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
