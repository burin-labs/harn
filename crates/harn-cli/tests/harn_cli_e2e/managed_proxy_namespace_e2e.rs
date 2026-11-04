//! A Linux child whose network stops at the managed proxy, on the canonical CLI
//! path. The server below is on the host's loopback, which the child's namespace
//! cannot see, so the only way its bytes can reach the server is the proxy.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use crate::test_util::process::{harn_e2e_binary, harn_e2e_command};

const REPLY: &str = "reached-through-proxy";

/// Set where the host grants the helper namespace authority, which requires the
/// proof instead of skipping it. Ubuntu refuses namespaces to any executable with
/// no AppArmor grant of its own, so an ordinary runner cannot run this.
const LAUNCHER_ENV: &str = "HARN_EXT_OWNER_DEATH_NETNS_LAUNCHER";

const PROBE: &str = r"
import errno, os, socket, sys, urllib.error, urllib.request
port, blocked_port, unix_path, v6_port = int(sys.argv[1]), int(sys.argv[3]), sys.argv[4], int(sys.argv[5])
assert os.environ['HTTP_PROXY'].startswith('http://127.0.0.1:'), os.environ['HTTP_PROXY']
opener = urllib.request.build_opener(urllib.request.ProxyHandler({'http': os.environ['HTTP_PROXY']}))
with opener.open('http://127.0.0.1:%d/secret' % port, timeout=20) as reply:
    assert reply.read().decode() == sys.argv[2]
def refused_by_proxy(url):
    try:
        opener.open(url, timeout=20)
    except urllib.error.HTTPError as error:
        assert error.code == 403, error.code
    else:
        raise AssertionError('the proxy admitted ' + url)
refused_by_proxy('http://127.0.0.2:%d/' % blocked_port)
if v6_port:
    refused_by_proxy('http://[::1]:%d/' % v6_port)
for host, target in [('127.0.0.1', port), ('127.0.0.2', blocked_port)]:
    direct = socket.socket()
    try:
        direct.connect((host, target))
    except OSError as error:
        assert error.errno == errno.ECONNREFUSED, error
    else:
        raise AssertionError('a raw socket reached the host server past the proxy')
    direct.close()
for family, address in [(socket.AF_INET, ('198.51.100.1', 9)), (socket.AF_INET6, ('2001:db8::1', 9))]:
    for kind in [socket.SOCK_STREAM, socket.SOCK_DGRAM]:
        remote = socket.socket(family, kind)
        try:
            if kind == socket.SOCK_STREAM:
                remote.connect(address)
            else:
                remote.sendto(b'no egress', address)
        except OSError as error:
            assert error.errno in (errno.ENETUNREACH, errno.EADDRNOTAVAIL), error
        else:
            raise AssertionError('a raw socket left the namespace')
        remote.close()
if v6_port:
    direct = socket.socket(socket.AF_INET6)
    try:
        direct.connect(('::1', v6_port))
    except OSError as error:
        assert error.errno in (errno.ECONNREFUSED, errno.ENETUNREACH, errno.EADDRNOTAVAIL), error
    else:
        raise AssertionError('an IPv6 socket reached the host server past the proxy')
    direct.close()
assert os.path.exists(unix_path), 'the socket file must be visible, or this proves nothing'
try:
    unix = socket.socket(socket.AF_UNIX)
    unix.connect(unix_path)
except PermissionError as error:
    assert error.errno == errno.EPERM, error
else:
    raise AssertionError('a Unix socket reached a host service past the proxy')
left, right = socket.socketpair()
left.send(b'x')
assert right.recv(1) == b'x'
print('proxy-only-namespace-ok', flush=True)
";

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

    // Listeners the child must not reach, each counting what arrives. An
    // unresolvable name would read as refused even from a proxy admitting
    // everything; these answer anyone who actually connects.
    let blocked = TcpListener::bind(("127.0.0.2", 0)).unwrap();
    let blocked_port = blocked.local_addr().unwrap().port();
    let blocked_requests = Arc::new(AtomicUsize::new(0));
    let blocked_served = Arc::clone(&blocked_requests);
    std::thread::spawn(move || serve(blocked, blocked_served));
    let v6_requests = Arc::new(AtomicUsize::new(0));
    let v6_port = TcpListener::bind(("::1", 0)).ok().map(|v6| {
        let port = v6.local_addr().unwrap().port();
        let served = Arc::clone(&v6_requests);
        std::thread::spawn(move || serve(v6, served));
        port
    });
    assert!(
        configured.is_none() || v6_port.is_some(),
        "{LAUNCHER_ENV} requires this proof, and this host has no IPv6 loopback"
    );

    let root = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let unix_path = root.path().join("host-service.sock");
    let unix = UnixListener::bind(&unix_path).unwrap();
    let unix_connections = Arc::new(AtomicUsize::new(0));
    let unix_seen = Arc::clone(&unix_connections);
    std::thread::spawn(move || {
        for _ in unix.incoming().flatten() {
            unix_seen.fetch_add(1, Ordering::SeqCst);
        }
    });

    // The controls for the negatives below: from the host, each of these answers.
    for address in [
        Some(format!("127.0.0.1:{port}")),
        Some(format!("127.0.0.2:{blocked_port}")),
        v6_port.map(|port| format!("[::1]:{port}")),
    ]
    .into_iter()
    .flatten()
    {
        let mut control = std::net::TcpStream::connect(&address).unwrap();
        control.write_all(b"control\r\n\r\n").unwrap();
        let mut reply = String::new();
        control.read_to_string(&mut reply).unwrap();
        assert!(reply.ends_with(REPLY), "{address}: {reply}");
    }
    assert_eq!(requests.swap(0, Ordering::SeqCst), 1);
    assert_eq!(blocked_requests.swap(0, Ordering::SeqCst), 1);
    v6_requests.store(0, Ordering::SeqCst);
    drop(UnixStream::connect(&unix_path).unwrap());
    while unix_connections.load(Ordering::SeqCst) == 0 {
        std::thread::yield_now();
    }
    unix_connections.store(0, Ordering::SeqCst);

    let source = format!(
        r#"
fn main(harness: Harness) {{
  harness.net.egress_policy({{default: "deny", allow: ["127.0.0.1"], allow_loopback: true}})
  const result = harness.tools.run_command({{argv: ["/usr/bin/python3", "-c", {probe}, "{port}", "{REPLY}", "{blocked_port}", {unix_path}, "{v6}"]}})
  harness.stdio.println(json_stringify(result))
  assert_eq(result.exit_code, 0)
  assert_eq(trim(result.stdout), "proxy-only-namespace-ok")
  assert_eq(result.sandbox.network.managed_proxy, true)
  assert_eq(result.sandbox.network.direct_network, false)
}}
"#,
        probe = serde_json::to_string(PROBE).unwrap(),
        unix_path = serde_json::to_string(&unix_path).unwrap(),
        v6 = v6_port.unwrap_or(0),
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
    let admitted = requests.load(Ordering::SeqCst);
    let second_listener = blocked_requests.load(Ordering::SeqCst);
    let ipv6 = v6_requests.load(Ordering::SeqCst);
    let unix_socket = unix_connections.load(Ordering::SeqCst);
    eprintln!(
        "PROOF-RECEIPT uid={} admitted={admitted} second_listener={second_listener} \
         ipv6={ipv6} ipv6_measured={} unix_socket={unix_socket}",
        std::fs::metadata("/proc/self").unwrap().uid(),
        v6_port.is_some()
    );
    assert_eq!(admitted, 1, "only the admitted request reaches the server");
    assert_eq!(
        second_listener, 0,
        "the second listener must receive nothing"
    );
    assert_eq!(ipv6, 0, "the IPv6 listener must receive nothing");
    assert_eq!(unix_socket, 0, "the host socket must receive nothing");
}
