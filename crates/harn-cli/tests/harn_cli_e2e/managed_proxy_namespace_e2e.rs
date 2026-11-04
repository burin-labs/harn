//! A Linux child whose network stops at the managed proxy, on the canonical CLI
//! path. The server below is on the host's loopback, which the child's namespace
//! cannot see, so the only way its bytes can reach the server is the proxy.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use crate::test_util::process::{harn_e2e_binary, harn_e2e_command};

const REPLY: &str = "reached-through-proxy";

/// Set where the host grants the helper namespace authority, which requires the
/// proof instead of skipping it. Ubuntu refuses namespaces to any executable with
/// no AppArmor grant of its own, so an ordinary runner cannot run this.
const LAUNCHER_ENV: &str = "HARN_EXT_OWNER_DEATH_NETNS_LAUNCHER";

const PROBE: &str = r#"
import errno, os, socket, sys, urllib.error, urllib.request
port = int(sys.argv[1])
assert os.environ['HTTP_PROXY'].startswith('http://127.0.0.1:'), os.environ['HTTP_PROXY']
opener = urllib.request.build_opener(urllib.request.ProxyHandler({'http': os.environ['HTTP_PROXY']}))
with opener.open('http://127.0.0.1:%d/secret' % port, timeout=20) as reply:
    assert reply.read().decode() == sys.argv[2]
try:
    opener.open('http://refused.example/', timeout=20)
except urllib.error.HTTPError as error:
    assert error.code == 403, error.code
else:
    raise AssertionError('the proxy admitted an unlisted host')
direct = socket.socket()
try:
    direct.connect(('127.0.0.1', port))
except OSError as error:
    assert error.errno == errno.ECONNREFUSED, error
else:
    raise AssertionError('a raw socket reached the host server past the proxy')
direct.close()
for kind in [socket.SOCK_STREAM, socket.SOCK_DGRAM]:
    remote = socket.socket(socket.AF_INET, kind)
    try:
        if kind == socket.SOCK_STREAM:
            remote.connect(('198.51.100.1', 9))
        else:
            remote.sendto(b'no egress', ('198.51.100.1', 9))
    except OSError as error:
        assert error.errno == errno.ENETUNREACH, error
    else:
        raise AssertionError('a raw socket left the namespace')
    remote.close()
print('proxy-only-namespace-ok', flush=True)
"#;

fn namespace_authority(launcher: &str) -> bool {
    Command::new(launcher)
        .args([
            "netns-launch",
            "--seccomp-hex",
            "060000000000ff7f",
            "--",
            "/usr/bin/python3",
            "-c",
            "import ctypes; assert ctypes.CDLL(None).unshare(0) == 0",
        ])
        .output()
        .is_ok_and(|output| output.status.success())
}

/// Count every request and answer each one, so a request the proxy never
/// carried cannot be mistaken for a pass.
fn serve(listener: TcpListener, requests: Arc<AtomicUsize>) {
    for stream in listener.incoming().flatten() {
        let mut stream = stream;
        let mut head = [0_u8; 2048];
        let _ = stream.read(&mut head);
        requests.fetch_add(1, Ordering::SeqCst);
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{REPLY}",
            REPLY.len()
        );
    }
}

#[test]
fn a_child_reaches_only_what_the_proxy_admits() {
    let configured = std::env::var(LAUNCHER_ENV).ok();
    let launcher = configured
        .clone()
        .unwrap_or_else(|| harn_e2e_binary().display().to_string());
    if !harn_vm::process_sandbox::active_backend_filesystem_available()
        || !namespace_authority(&launcher)
    {
        assert!(
            configured.is_none(),
            "{LAUNCHER_ENV} requires this proof, and Landlock or namespace authority is missing"
        );
        eprintln!(
            "NOT EXERCISED: Landlock or namespace authority unavailable; set {LAUNCHER_ENV} to \
             require the proof."
        );
        return;
    }
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let requests = Arc::new(AtomicUsize::new(0));
    let served = Arc::clone(&requests);
    std::thread::spawn(move || serve(listener, served));

    // The control for the raw connect below: from the host, this port answers.
    let mut control = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    control.write_all(b"control\r\n\r\n").unwrap();
    let mut reply = String::new();
    control.read_to_string(&mut reply).unwrap();
    assert!(reply.ends_with(REPLY), "{reply}");
    assert_eq!(requests.swap(0, Ordering::SeqCst), 1);

    let root = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let source = format!(
        r#"
fn main(harness: Harness) {{
  harness.net.egress_policy({{default: "deny", allow: ["127.0.0.1"], allow_loopback: true}})
  const result = harness.tools.run_command({{argv: ["/usr/bin/python3", "-c", {probe}, "{port}", "{REPLY}"]}})
  harness.stdio.println(json_stringify(result))
  assert_eq(result.exit_code, 0)
  assert_eq(trim(result.stdout), "proxy-only-namespace-ok")
  assert_eq(result.sandbox.network.managed_proxy, true)
  assert_eq(result.sandbox.network.direct_network, false)
}}
"#,
        probe = serde_json::to_string(PROBE).unwrap(),
    );
    let output = harn_e2e_command()
        .current_dir(root.path())
        .args([
            "run",
            "--standalone",
            "--allow-process-network",
            "--netns-launcher",
            &launcher,
            "-e",
            &source,
        ])
        .output()
        .expect("run the canonical CLI");
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        requests.load(Ordering::SeqCst),
        1,
        "only the admitted request reaches the server"
    );
}
