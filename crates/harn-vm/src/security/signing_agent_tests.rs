//! The filter against a real `ssh-agent`, driven by OpenSSH's own clients.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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

fn with_agent(socket: &Path, program: &str, args: &[&str]) -> Output {
    Command::new(program)
        .args(args)
        .env("SSH_AUTH_SOCK", socket)
        .output()
        .expect("run OpenSSH client")
}

fn public(key: &Path) -> String {
    std::fs::read_to_string(key.with_extension("pub")).expect("public key")
}

/// One raw request to `socket`, returning the reply's message type.
fn raw_request(socket: &Path, body: &[u8]) -> u8 {
    let mut stream = UnixStream::connect(socket).expect("connect");
    write_message(&mut stream, body).expect("write");
    read_message(&mut stream).expect("read").expect("reply")[0]
}

fn sign_request(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut body = vec![SIGN_REQUEST];
    put_string(&mut body, key);
    put_string(&mut body, data);
    body.extend(0u32.to_be_bytes());
    body
}

#[test]
fn only_the_configured_key_signs_and_only_for_its_namespace() {
    let dir = tempfile::tempdir().expect("dir");
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
    let dir = tempfile::tempdir().expect("dir");
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
