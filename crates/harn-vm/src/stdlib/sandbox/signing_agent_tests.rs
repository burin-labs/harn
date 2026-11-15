//! The filter against a real `ssh-agent`, driven by OpenSSH's own clients.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use super::*;

/// A daemonized `ssh-agent` on a private socket, killed on drop. Without
/// `-D` the agent returns only after its socket is bound.
struct Upstream {
    socket: PathBuf,
    pid: String,
}

impl Upstream {
    fn start(dir: &Path) -> Self {
        let socket = dir.join("upstream.sock");
        let output = Command::new("ssh-agent")
            .arg("-s")
            .arg("-a")
            .arg(&socket)
            .output()
            .expect("start ssh-agent");
        assert!(output.status.success(), "ssh-agent: {output:?}");
        let text = String::from_utf8_lossy(&output.stdout);
        let pid = text
            .split(';')
            .find_map(|part| part.trim().strip_prefix("SSH_AGENT_PID="))
            .expect("agent pid")
            .to_string();
        Self { socket, pid }
    }
}

impl Drop for Upstream {
    fn drop(&mut self) {
        let _ = Command::new("kill").arg(&self.pid).status();
    }
}

/// A scratch directory private to this user, as the filter requires.
fn private_dir() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().expect("dir");
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).expect("chmod");
    dir
}

fn keygen(dir: &Path, name: &str) -> PathBuf {
    let key = dir.join(name);
    let status = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-C", name, "-f"])
        .arg(&key)
        .status()
        .expect("ssh-keygen");
    assert!(status.success());
    key
}

/// Run an OpenSSH client against `socket`. A filter that never answers fails
/// the test at the shared hang ceiling instead of hanging it.
fn with_agent(socket: &Path, program: &str, args: &[&str]) -> Output {
    let child = Command::new(program)
        .args(args)
        .env("SSH_AUTH_SOCK", socket)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run OpenSSH client");
    let (answered, answer) = std::sync::mpsc::channel();
    crate::runtime_stack::spawn(move || {
        let _ = answered.send(child.wait_with_output());
    });
    harn_clock::test_support::recv_within(&format!("{program} {args:?} answered"), &answer)
        .expect("client output")
}

fn soon() -> Instant {
    Instant::now() + Duration::from_secs(10)
}

fn public(key: &Path) -> String {
    std::fs::read_to_string(key.with_extension("pub")).expect("public key")
}

/// One raw request to `socket`, returning the reply's message type.
fn raw_request(socket: &Path, body: &[u8]) -> u8 {
    let mut stream = UnixStream::connect(socket).expect("connect");
    write_message(&mut stream, body, soon()).expect("write");
    read_message(&mut stream, soon())
        .expect("read")
        .expect("reply")[0]
}

fn sign_request(key: &[u8], data: &[u8]) -> Vec<u8> {
    sign_request_with_flags(key, data, 0)
}

fn sign_request_with_flags(key: &[u8], data: &[u8], flags: u32) -> Vec<u8> {
    let mut body = vec![SIGN_REQUEST];
    put_string(&mut body, key);
    put_string(&mut body, data);
    body.extend(flags.to_be_bytes());
    body
}

#[test]
fn only_the_configured_key_signs_and_only_for_its_namespace() {
    let dir = private_dir();
    let dir = dir.path().canonicalize().expect("canonical");
    let signing = keygen(&dir, "signing");
    let other = keygen(&dir, "other");
    let upstream = Upstream::start(&dir);
    for key in [&signing, &other] {
        let added = with_agent(&upstream.socket, "ssh-add", &[key.to_str().unwrap()]);
        assert!(added.status.success(), "ssh-add: {added:?}");
        // `ssh-keygen -Y sign` falls back to the private key file for a key
        // the agent does not list; a confined signer cannot read it either.
        std::fs::remove_file(key).expect("remove private key");
    }
    let filter = SigningAgent::serve(
        &dir,
        &upstream.socket,
        SigningScope {
            public_key: public(&signing),
            namespace: "git".into(),
        },
    )
    .expect("serve");
    let socket = filter.socket();

    let listed = with_agent(socket, "ssh-add", &["-L"]);
    let listed = String::from_utf8_lossy(&listed.stdout);
    assert_eq!(listed.trim(), public(&signing).trim(), "listing: {listed}");

    let message = dir.join("message");
    std::fs::write(&message, "tree 0\n").expect("message");
    let sign = |key: &Path, namespace: &str| {
        let _ = std::fs::remove_file(message.with_extension("sig"));
        with_agent(
            socket,
            "ssh-keygen",
            &[
                "-Y",
                "sign",
                "-n",
                namespace,
                "-f",
                key.with_extension("pub").to_str().unwrap(),
                message.to_str().unwrap(),
            ],
        )
    };
    let signed = sign(&signing, "git");
    assert!(signed.status.success(), "git signature refused: {signed:?}");
    let signature = std::fs::read(message.with_extension("sig")).expect("signature");
    let checked = Command::new("ssh-keygen")
        .args(["-Y", "check-novalidate", "-n", "git", "-s"])
        .arg(message.with_extension("sig"))
        .stdin(std::fs::File::open(&message).expect("open"))
        .output()
        .expect("check");
    assert!(
        checked.status.success(),
        "signature does not verify: {checked:?}"
    );
    assert!(!signature.is_empty());

    assert!(
        !sign(&signing, "file").status.success(),
        "signed another namespace"
    );
    assert!(
        !sign(&other, "git").status.success(),
        "signed with another key"
    );

    // An authentication signature: session id first, never the SSHSIG magic.
    // The upstream agent signs it, so only the filter refuses it.
    let key = key_blob(&public(&signing)).expect("blob");
    let mut auth = Vec::new();
    put_string(&mut auth, &[7u8; 32]);
    auth.push(50);
    put_string(&mut auth, b"git");
    assert_eq!(
        raw_request(&upstream.socket, &sign_request(&key, &auth)),
        SIGN_RESPONSE
    );
    assert_eq!(raw_request(socket, &sign_request(&key, &auth)), FAILURE);

    // A namespace payload with bytes after it is not an SSHSIG payload.
    let mut sshsig = b"SSHSIG".to_vec();
    for field in [b"git".as_slice(), b"", b"sha512", &[0u8; 64]] {
        put_string(&mut sshsig, field);
    }
    assert_eq!(
        raw_request(socket, &sign_request(&key, &sshsig)),
        SIGN_RESPONSE
    );
    // A flag beyond the RSA SHA-2 hash choices is refused, though the
    // upstream agent would sign with it.
    assert_eq!(
        raw_request(&upstream.socket, &sign_request_with_flags(&key, &sshsig, 1)),
        SIGN_RESPONSE
    );
    for flags in [1, 6, 8] {
        assert_eq!(
            raw_request(socket, &sign_request_with_flags(&key, &sshsig, flags)),
            FAILURE,
            "flags {flags}"
        );
    }
    sshsig.push(0);
    assert_eq!(raw_request(socket, &sign_request(&key, &sshsig)), FAILURE);

    let removed = with_agent(socket, "ssh-add", &["-D"]);
    assert!(!removed.status.success(), "removed keys through the filter");
    let added = with_agent(socket, "ssh-add", &[other.to_str().unwrap()]);
    assert!(!added.status.success(), "added a key through the filter");
    let upstream_keys = with_agent(&upstream.socket, "ssh-add", &["-L"]);
    assert_eq!(
        String::from_utf8_lossy(&upstream_keys.stdout)
            .lines()
            .count(),
        2
    );
}

#[test]
fn a_key_the_upstream_agent_lacks_is_not_listed() {
    let dir = private_dir();
    let dir = dir.path().canonicalize().expect("canonical");
    let signing = keygen(&dir, "signing");
    let upstream = Upstream::start(&dir);
    let filter = SigningAgent::serve(
        &dir,
        &upstream.socket,
        SigningScope {
            public_key: public(&signing),
            namespace: "git".into(),
        },
    )
    .expect("serve");
    let listed = with_agent(filter.socket(), "ssh-add", &["-L"]);
    assert!(
        !listed.status.success(),
        "listed a key the agent lacks: {listed:?}"
    );
    assert!(!String::from_utf8_lossy(&listed.stdout).contains("ssh-ed25519"));
}

/// A filter with the signing key loaded upstream, in a private directory.
struct Served {
    _dir: tempfile::TempDir,
    signing: PathBuf,
    _upstream: Upstream,
    filter: SigningAgent,
}

fn served() -> Served {
    let dir = private_dir();
    let path = dir.path().canonicalize().expect("canonical");
    let signing = keygen(&path, "signing");
    let upstream = Upstream::start(&path);
    let added = with_agent(&upstream.socket, "ssh-add", &[signing.to_str().unwrap()]);
    assert!(added.status.success(), "ssh-add: {added:?}");
    let filter = SigningAgent::serve(
        &path,
        &upstream.socket,
        SigningScope {
            public_key: public(&signing),
            namespace: "git".into(),
        },
    )
    .expect("serve");
    Served {
        _dir: dir,
        signing,
        _upstream: upstream,
        filter,
    }
}

/// One client that pipelines requests without reading a reply, and one that
/// trickles a request a byte at a time and never finishes it, each hold only
/// their own connection: another client is answered after every trickled
/// byte, and dropping the filter returns at once instead of waiting on them.
#[test]
fn a_stalled_client_neither_blocks_another_nor_holds_teardown() {
    let Served {
        _dir,
        signing,
        _upstream,
        filter,
    } = served();
    let socket = filter.socket().to_path_buf();
    let lists_the_key = |when: &str| {
        let listed = with_agent(&socket, "ssh-add", &["-L"]);
        assert_eq!(
            String::from_utf8_lossy(&listed.stdout).trim(),
            public(&signing).trim(),
            "{when}: another client went unanswered: {listed:?}"
        );
    };

    // Unknown requests are answered locally, so the replies this client never
    // reads fill its socket and leave the filter blocked writing to it.
    let mut flooding = UnixStream::connect(&socket).expect("connect");
    let flood = crate::runtime_stack::spawn(move || {
        let request = [0, 0, 0, 1, 99];
        let mut sent = 0usize;
        while flooding.write_all(&request).is_ok() {
            sent += 1;
        }
        sent
    });
    // A request header, then a body byte that never comes.
    let mut trickling = UnixStream::connect(&socket).expect("connect");
    for (index, byte) in [0u8, 0, 0, 1].into_iter().enumerate() {
        trickling.write_all(&[byte]).expect("trickle");
        lists_the_key(&format!("after trickled byte {index}"));
    }

    let started = Instant::now();
    drop(filter);
    let teardown = started.elapsed();
    assert!(
        teardown < Duration::from_secs(2),
        "dropping the filter waited {teardown:?} on stalled clients"
    );
    assert!(!socket.exists(), "the socket outlived the filter");
    // The shutdown ends the flooding client's writes and the trickle's
    // connection.
    let sent = flood.join().expect("flood");
    assert!(sent > 0, "the flooding client never sent a request");
    let mut rest = Vec::new();
    assert_eq!(trickling.read_to_end(&mut rest).expect("closed"), 0);
}

#[test]
fn the_socket_directory_must_be_private_and_a_stale_socket_is_replaced() {
    use std::os::unix::fs::PermissionsExt as _;
    let Served {
        _dir,
        signing,
        _upstream,
        filter,
    } = served();
    let dir = filter.socket().parent().unwrap().to_path_buf();
    let upstream = dir.join("upstream.sock");
    let scope = || SigningScope {
        public_key: public(&signing),
        namespace: "git".into(),
    };

    // A live filter's socket is not taken over.
    let error = SigningAgent::serve(&dir, &upstream, scope())
        .err()
        .expect("refused");
    assert_eq!(error.kind(), std::io::ErrorKind::AddrInUse, "{error}");

    // A socket nothing serves any more is replaced.
    drop(filter);
    drop(UnixListener::bind(dir.join(SOCKET)).expect("bind a stale socket"));
    assert!(dir.join(SOCKET).exists());
    let replaced = SigningAgent::serve(&dir, &upstream, scope()).expect("replace the stale socket");
    let listed = with_agent(replaced.socket(), "ssh-add", &["-L"]);
    assert!(listed.status.success(), "{listed:?}");
    drop(replaced);

    // A directory others can open is refused.
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o750)).expect("chmod");
    let error = SigningAgent::serve(&dir, &upstream, scope())
        .err()
        .expect("refused");
    assert_eq!(
        error.kind(),
        std::io::ErrorKind::PermissionDenied,
        "{error}"
    );
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("chmod");
}
