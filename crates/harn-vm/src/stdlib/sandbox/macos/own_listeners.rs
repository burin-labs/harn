//! Which local listeners a command behind the managed proxy may connect to.
//!
//! A loopback grant exists so a command can run a local server and connect to
//! it: test suites, dev servers, browsers driving a dev server. Seatbelt names
//! a destination only as `localhost:<port>` and cannot tell whose listener is
//! on the other end, so the profile is computed at spawn instead. It admits
//! every local port, then denies each port that a foreign socket holds at that
//! moment, then re-admits the proxy's ports. A socket is this run's own only
//! when a process carrying this runtime's lineage marker
//! ([`super::super::process_output::CHILD_LINEAGE_ENV`]) holds it. Every child
//! behind the proxy inherits the marker, and a dev server an earlier command
//! left running keeps it. The marker is a label, not a secret.
//!
//! The socket table is the kernel's, for every user and address:
//! `net.inet.tcp.pcblist64` and `net.inet.udp.pcblist64`. Each row carries the
//! kernel's socket identity, and `libproc` reports the same identity for the
//! sockets a marked process holds. A port is admitted only when every row on
//! it is such a socket. A row nothing accounts for, including one held by a
//! process or descriptor that cannot be read, denies its port. Both readers
//! check themselves on every scan against sockets this process just opened,
//! and the rule shape is checked under each distinct base profile. When a
//! check fails, the command gets the strict proxy-only profile instead.
//!
//! What this leaves open: a profile is fixed at exec, so a service that starts
//! listening later is reachable from every process that started before it. A
//! marked server that keeps running extends that window to every later command
//! that talks to it.

use std::collections::{BTreeSet, HashMap};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, TcpListener, UdpSocket};

use crate::orchestration::CapabilityPolicy;

/// Profile rules for a loopback grant behind the proxy, appended to
/// `base_profile`. `Err` names why they could not be trusted.
pub(super) fn rules(
    policy: &CapabilityPolicy,
    base_profile: &str,
) -> Option<Result<String, String>> {
    let grant = super::super::policy::ProcessNetworkGrant::for_policy(policy);
    let proxy = policy
        .process_network_proxy
        .filter(|_| grant.loopback_behind_proxy())?;
    let proxy_ports = [proxy.http_port, proxy.socks_port];
    Some(
        shape_holds(base_profile)
            .and_then(|()| foreign_ports())
            .map(|foreign| render(foreign, proxy_ports)),
    )
}

/// Admit every local port, deny `denied`, then re-admit the proxy's ports over
/// IPv4 TCP only.
///
/// The shape is what Seatbelt was measured to enforce, which its rule order
/// alone does not predict: a port deny after a `localhost:*` allow is ignored
/// unless a path rule comes first, hence the inert anchor; and after a `tcp`
/// deny, a `tcp4` allow on the same port re-admits IPv6, which an `ip` deny
/// prevents. [`shape_holds`] checks it on this host.
fn render(denied: impl IntoIterator<Item = u16>, proxy_ports: [u16; 2]) -> String {
    let mut denied: BTreeSet<u16> = denied.into_iter().collect();
    denied.extend(proxy_ports);
    let mut rules = String::from(
        "(allow network-outbound (literal \"/dev/null\"))\n\
         (allow network-outbound (remote ip \"localhost:*\"))\n",
    );
    for port in denied {
        rules.push_str(&format!(
            "(deny network-outbound (remote ip \"localhost:{port}\"))\n"
        ));
    }
    for port in proxy_ports {
        rules.push_str(&format!(
            "(allow network-outbound (remote tcp4 \"localhost:{port}\"))\n"
        ));
    }
    rules
}

/// Whether [`render`]'s rules, appended to `base_profile`, deny what they name
/// on this host. Measured once per [`shape_key`], and at most
/// [`SHAPE_CACHE_LIMIT`] keys are kept.
fn shape_holds(base_profile: &str) -> Result<(), String> {
    type Measured = std::sync::Mutex<HashMap<String, Result<(), String>>>;
    static MEASURED: std::sync::OnceLock<Measured> = std::sync::OnceLock::new();
    let key = shape_key(base_profile);
    let cache = MEASURED.get_or_init(Default::default);
    if let Some(known) = cache.lock().map_err(|e| e.to_string())?.get(&key) {
        return known.clone();
    }
    let measured = measure_shape(base_profile);
    let mut cache = cache.lock().map_err(|e| e.to_string())?;
    if cache.len() >= SHAPE_CACHE_LIMIT {
        cache.clear();
    }
    cache.insert(key, measured.clone());
    measured
}

const SHAPE_CACHE_LIMIT: usize = 32;

/// What the measured quirks depend on: which rules a profile has and in what
/// order. Paths and port numbers vary per spawn (its own directory, workspace
/// and toolchain roots, the proxy's ports) without changing that, so every
/// quoted path becomes `PATH` and every port on a `localhost` endpoint `N`. A
/// wildcard endpoint stays as written, since a port rule and a wildcard rule
/// behave differently.
fn shape_key(profile: &str) -> String {
    // Splitting on quotes alternates text outside a literal with a literal.
    profile
        .split('"')
        .enumerate()
        .map(|(index, part)| {
            if index % 2 == 0 {
                return part;
            }
            match part.split_once(':') {
                Some((host, "*")) if !host.contains('/') => part,
                Some((host, port))
                    if !host.contains('/') && port.bytes().all(|byte| byte.is_ascii_digit()) =>
                {
                    if host == "*" {
                        "*:N"
                    } else {
                        "localhost:N"
                    }
                }
                _ => "PATH",
            }
        })
        .collect::<Vec<_>>()
        .join("\"")
}

/// Run real clients under `base_profile` plus [`render`]'s rules against
/// sockets this process holds. Every denied kind must be refused, and the
/// proxy, an undenied listener and an undenied socket must be reached, so the
/// check passes neither by refusing everything nor by admitting everything.
fn measure_shape(base_profile: &str) -> Result<(), String> {
    let tcp = |address: (IpAddr, u16)| {
        TcpListener::bind(address).map_err(|error| format!("shape probe listener: {error}"))
    };
    let udp = |address: (IpAddr, u16)| {
        let socket =
            UdpSocket::bind(address).map_err(|error| format!("shape probe socket: {error}"))?;
        socket
            .set_nonblocking(true)
            .map_err(|error| format!("shape probe socket: {error}"))?;
        Ok::<_, String>(socket)
    };
    let v4 = IpAddr::from(Ipv4Addr::LOCALHOST);
    let v6 = IpAddr::from(Ipv6Addr::LOCALHOST);
    let port = |listener: &TcpListener| listener.local_addr().map(|a| a.port()).unwrap_or(0);
    let udp_port = |socket: &UdpSocket| socket.local_addr().map(|a| a.port()).unwrap_or(0);
    let denied_v4 = tcp((v4, 0))?;
    let denied_v6 = tcp((v6, 0))?;
    let denied_udp = udp((v4, 0))?;
    let open_v4 = tcp((v4, 0))?;
    let open_udp = udp((v4, 0))?;
    let proxy = tcp((v4, 0))?;
    let proxy_v6 = tcp((v6, port(&proxy)))?;
    let lan = if_addrs::get_if_addrs()
        .ok()
        .into_iter()
        .flatten()
        .map(|interface| interface.ip())
        .find(|address| address.is_ipv4() && !address.is_loopback());
    let denied_lan = lan.map(|address| tcp((address, 0))).transpose()?;
    let mut denied = vec![port(&denied_v4), port(&denied_v6), udp_port(&denied_udp)];
    denied.extend(denied_lan.as_ref().map(port));
    let profile = format!("{base_profile}{}", render(denied, [port(&proxy); 2]));

    let sandboxed = |program: &str, args: &[&str]| {
        std::process::Command::new(super::SANDBOX_EXEC_PATH)
            .env_clear()
            .args(["-p", &profile, program])
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|status| status.success())
            .map_err(|error| format!("shape probe: {error}"))
    };
    let connects = |host: IpAddr, port: u16| {
        sandboxed(
            "/usr/bin/nc",
            &[
                "-z",
                "-G",
                "2",
                "-w",
                "2",
                &host.to_string(),
                &port.to_string(),
            ],
        )
    };
    // A datagram is reached only if it arrived; loopback delivery completes
    // inside `sendto`, so it is queued by the time the client exits.
    let delivers = |socket: &UdpSocket| {
        let send = format!("printf x > /dev/udp/127.0.0.1/{}", udp_port(socket));
        sandboxed("/bin/bash", &["-c", &send])?;
        Ok::<_, String>(socket.recv_from(&mut [0u8; 8]).is_ok())
    };
    let mut measured = vec![
        (
            "a denied IPv4 listener",
            connects(v4, port(&denied_v4))?,
            false,
        ),
        (
            "a denied IPv6 listener",
            connects(v6, port(&denied_v6))?,
            false,
        ),
        ("a denied UDP socket", delivers(&denied_udp)?, false),
        ("the proxy over IPv4", connects(v4, port(&proxy))?, true),
        (
            "the proxy port over IPv6",
            connects(v6, port(&proxy_v6))?,
            false,
        ),
        ("an undenied listener", connects(v4, port(&open_v4))?, true),
        ("an undenied UDP socket", delivers(&open_udp)?, true),
    ];
    if let (Some(address), Some(listener)) = (lan, &denied_lan) {
        measured.push((
            "a denied listener at a LAN address",
            connects(address, port(listener))?,
            false,
        ));
    }
    match measured
        .iter()
        .find(|(_, reached, expected)| reached != expected)
    {
        Some((target, reached, _)) => Err(format!(
            "the per-port profile {} {target} on this host",
            if *reached { "reached" } else { "did not reach" }
        )),
        None => Ok(()),
    }
}

/// Say once per process that a command behind the proxy got the strict
/// profile, which reaches no local server, and why.
pub(super) fn report_strict(reason: &str) {
    static REPORTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if REPORTED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    crate::events::log_warn_meta(
        crate::process_sandbox::LOOPBACK_BEHIND_PROXY_CATEGORY,
        &format!(
            "On macOS, a confined command with loopback networking behind the managed network \
             proxy connects only through the proxy, to its own local servers too, because the \
             local listener check failed: {reason}"
        ),
        std::collections::BTreeMap::from([
            (
                "grant".to_string(),
                crate::process_sandbox::LOOPBACK_GRANT_FIELD.into(),
            ),
            ("reason".to_string(), reason.into()),
        ]),
    );
}

/// Ports held now by a socket that no marked process accounts for.
fn foreign_ports() -> Result<BTreeSet<u16>, String> {
    let me = unsafe { libc::getuid() };
    let probes = Probes::open()?;
    let mut rows = table::rows(table::Protocol::Tcp)?;
    let udp_rows = table::rows(table::Protocol::Udp)?;
    probes.vouch(&rows, &udp_rows, me)?;
    rows.extend(udp_rows);
    let marked = owners::marked_sockets(me)?;
    Ok(rows
        .into_iter()
        .filter(|row| !marked.contains(&row.socket))
        .map(|row| row.port)
        .collect())
}

/// Sockets this process opens so both readers can be checked against known
/// facts: two IPv4 listeners, an IPv6-only listener and a UDP socket.
struct Probes {
    tcp: [TcpListener; 2],
    tcp6: TcpListener,
    udp: UdpSocket,
}

impl Probes {
    fn open() -> Result<Self, String> {
        let tcp =
            |address: &str| TcpListener::bind(address).map_err(|e| format!("probe listener: {e}"));
        Ok(Self {
            tcp: [tcp("127.0.0.1:0")?, tcp("127.0.0.1:0")?],
            tcp6: tcp("[::1]:0")?,
            udp: UdpSocket::bind("127.0.0.1:0").map_err(|e| format!("probe socket: {e}"))?,
        })
    }

    /// Each probe must appear in the kernel table as this user's, with an
    /// identity `libproc` also reports for this process, and the identities
    /// must be nonzero and distinct. A reader that returns a constant (a
    /// zeroed field, a shifted layout) would otherwise match every socket and
    /// admit every port.
    fn vouch(&self, tcp: &[table::Row], udp: &[table::Row], me: u32) -> Result<(), String> {
        let own = owners::sockets(std::process::id() as i32)?;
        let port = |address: std::io::Result<std::net::SocketAddr>| {
            address
                .map(|address| address.port())
                .map_err(|e| e.to_string())
        };
        let identity = |rows: &[table::Row], port: u16| {
            rows.iter()
                .find(|row| row.port == port && row.uid == me && own.contains(&row.socket))
                .map(|row| row.socket)
        };
        let identities = [
            identity(tcp, port(self.tcp[0].local_addr())?),
            identity(tcp, port(self.tcp[1].local_addr())?),
            identity(tcp, port(self.tcp6.local_addr())?),
            identity(udp, port(self.udp.local_addr())?),
        ];
        let found: Option<Vec<u64>> = identities.into_iter().collect();
        let distinct = found
            .as_ref()
            .map(|ids| ids.iter().copied().collect::<BTreeSet<_>>())
            .filter(|ids| ids.len() == 4 && !ids.contains(&0));
        match distinct {
            Some(_) => Ok(()),
            None => Err(format!(
                "the kernel socket table and libproc did not identify this process's own \
                 sockets as four distinct, nonzero sockets: {found:?}"
            )),
        }
    }
}

/// The kernel's socket table, through the public `pcblist64` records.
mod table {
    pub(super) enum Protocol {
        Tcp,
        Udp,
    }

    /// One listening TCP or bound UDP socket.
    pub(super) struct Row {
        pub(super) port: u16,
        pub(super) uid: u32,
        /// The kernel's socket identity, as `libproc` also reports it.
        pub(super) socket: u64,
    }

    // Offsets in the packed SDK records (`netinet/tcp_var.h`, `in_pcb.h`,
    // `sys/socketvar.h`); the self-check in `foreign_ports` vouches for them.
    const XINPGEN_LEN: usize = 24;
    const INPCB_LPORT: usize = 18;
    const INPCB_SOCKET: usize = 144;
    const XSOCKET_SO: usize = 4;
    const XSOCKET_UID: usize = 104;
    const TCPCB_INPCB: usize = 4;
    const TCPCB_STATE: usize = 292;
    const TCPS_LISTEN: i32 = 1;

    pub(super) fn rows(protocol: Protocol) -> Result<Vec<Row>, String> {
        let name = match protocol {
            Protocol::Tcp => c"net.inet.tcp.pcblist64",
            Protocol::Udp => c"net.inet.udp.pcblist64",
        };
        let buffer = read_sysctl(name)?;
        let u32_at = |at: usize| -> Option<u32> {
            Some(u32::from_ne_bytes(buffer.get(at..at + 4)?.try_into().ok()?))
        };
        let u64_at = |at: usize| -> Option<u64> {
            Some(u64::from_ne_bytes(buffer.get(at..at + 8)?.try_into().ok()?))
        };
        let port_at = |at: usize| -> Option<u16> {
            Some(u16::from_be_bytes(buffer.get(at..at + 2)?.try_into().ok()?))
        };
        let truncated = "truncated socket record";
        let mut found = Vec::new();
        let mut at = u32_at(0).ok_or("empty socket table")? as usize;
        // Records follow the header; a trailing header ends the list.
        while let Some(len) = u32_at(at).map(|len| len as usize) {
            if len <= XINPGEN_LEN {
                break;
            }
            let inpcb = match protocol {
                Protocol::Tcp => at + TCPCB_INPCB,
                Protocol::Udp => at,
            };
            let listening = match protocol {
                Protocol::Tcp => u32_at(at + TCPCB_STATE).ok_or(truncated)? as i32 == TCPS_LISTEN,
                Protocol::Udp => true,
            };
            let port = port_at(inpcb + INPCB_LPORT).ok_or(truncated)?;
            let socket = inpcb + INPCB_SOCKET;
            if listening && port != 0 {
                found.push(Row {
                    port,
                    uid: u32_at(socket + XSOCKET_UID).ok_or(truncated)?,
                    socket: u64_at(socket + XSOCKET_SO).ok_or(truncated)?,
                });
            }
            at += len;
        }
        Ok(found)
    }

    fn read_sysctl(name: &std::ffi::CStr) -> Result<Vec<u8>, String> {
        // The table can grow between the size query and the read; retry with
        // room to spare before giving up.
        for _ in 0..3 {
            let mut size = 0usize;
            let status = unsafe {
                libc::sysctlbyname(
                    name.as_ptr(),
                    std::ptr::null_mut(),
                    &raw mut size,
                    std::ptr::null_mut(),
                    0,
                )
            };
            if status != 0 {
                return Err(format!(
                    "read {name:?}: {}",
                    std::io::Error::last_os_error()
                ));
            }
            size += size / 4 + 4096;
            let mut buffer = vec![0u8; size];
            let status = unsafe {
                libc::sysctlbyname(
                    name.as_ptr(),
                    buffer.as_mut_ptr().cast(),
                    &raw mut size,
                    std::ptr::null_mut(),
                    0,
                )
            };
            if status == 0 {
                buffer.truncate(size);
                return Ok(buffer);
            }
            if std::io::Error::last_os_error().raw_os_error() != Some(libc::ENOMEM) {
                return Err(format!(
                    "read {name:?}: {}",
                    std::io::Error::last_os_error()
                ));
            }
        }
        Err(format!("read {name:?}: the table kept growing"))
    }
}

/// Which sockets this user's marked processes hold, through `libproc`.
mod owners {
    use std::collections::BTreeSet;

    // `sys/proc_info.h`: `struct socket_fdinfo`, whose `socket_info` starts at
    // 24 and holds the socket identity 136 bytes in.
    const SOCKET_FDINFO_LEN: usize = 792;
    const SOI_SO: usize = 24 + 136;
    const PROX_FDTYPE_SOCKET: u32 = 2;
    const PROC_PIDFDSOCKETINFO: i32 = 3;

    /// Socket identities held by `uid`'s processes that carry this runtime's
    /// lineage marker. A process or descriptor that cannot be read adds
    /// nothing, so its sockets stay foreign.
    pub(super) fn marked_sockets(uid: u32) -> Result<BTreeSet<u64>, String> {
        let marker = format!(
            "{}={}\0",
            crate::stdlib::sandbox::process_output::CHILD_LINEAGE_ENV,
            crate::stdlib::sandbox::process_output::child_lineage()
        );
        let mut marked = BTreeSet::new();
        let mut args = Vec::new();
        for pid in all_pids()? {
            if pid_uid(pid) == Some(uid) && carries(pid, marker.as_bytes(), &mut args) {
                marked.extend(sockets(pid).unwrap_or_default());
            }
        }
        Ok(marked)
    }

    /// Identities of the sockets `pid` holds that could be read.
    pub(super) fn sockets(pid: i32) -> Result<BTreeSet<u64>, String> {
        let mut found = BTreeSet::new();
        for fd in socket_fds(pid)? {
            let mut info = vec![0u8; SOCKET_FDINFO_LEN];
            let read = unsafe {
                libc::proc_pidfdinfo(
                    pid,
                    fd,
                    PROC_PIDFDSOCKETINFO,
                    info.as_mut_ptr().cast(),
                    SOCKET_FDINFO_LEN as i32,
                )
            };
            if read as usize == SOCKET_FDINFO_LEN {
                found.insert(u64::from_ne_bytes(
                    info[SOI_SO..SOI_SO + 8].try_into().unwrap(),
                ));
            }
        }
        Ok(found)
    }

    fn socket_fds(pid: i32) -> Result<Vec<i32>, String> {
        let size =
            unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0) };
        if size <= 0 {
            return Err(format!(
                "list descriptors of {pid}: {}",
                std::io::Error::last_os_error()
            ));
        }
        let mut fds = vec![
            libc::proc_fdinfo {
                proc_fd: 0,
                proc_fdtype: 0
            };
            size as usize / 8 + 16
        ];
        let read = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDLISTFDS,
                0,
                fds.as_mut_ptr().cast(),
                (fds.len() * 8) as i32,
            )
        };
        if read <= 0 {
            return Err(format!(
                "list descriptors of {pid}: {}",
                std::io::Error::last_os_error()
            ));
        }
        fds.truncate(read as usize / 8);
        Ok(fds
            .into_iter()
            .filter(|fd| fd.proc_fdtype == PROX_FDTYPE_SOCKET)
            .map(|fd| fd.proc_fd)
            .collect())
    }

    fn all_pids() -> Result<Vec<i32>, String> {
        let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
        if count <= 0 {
            return Err(format!(
                "list processes: {}",
                std::io::Error::last_os_error()
            ));
        }
        let mut pids = vec![0i32; count as usize + 64];
        let read =
            unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), (pids.len() * 4) as i32) };
        if read <= 0 {
            return Err(format!(
                "list processes: {}",
                std::io::Error::last_os_error()
            ));
        }
        pids.truncate(read as usize);
        Ok(pids)
    }

    fn pid_uid(pid: i32) -> Option<u32> {
        let mut info: libc::proc_bsdshortinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_bsdshortinfo>() as i32;
        let read = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDT_SHORTBSDINFO,
                0,
                (&raw mut info).cast(),
                size,
            )
        };
        (read == size).then_some(info.pbsi_uid)
    }

    /// Whether `pid`'s arguments and environment contain `marker` exactly.
    fn carries(pid: i32, marker: &[u8], buffer: &mut Vec<u8>) -> bool {
        let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
        let mut size = 0usize;
        let status = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                3,
                std::ptr::null_mut(),
                &raw mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if status != 0 || size == 0 {
            return false;
        }
        buffer.resize(size, 0);
        let status = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                3,
                buffer.as_mut_ptr().cast(),
                &raw mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        status == 0
            && buffer[..size]
                .windows(marker.len())
                .any(|window| window == marker)
    }
}

#[cfg(test)]
mod tests {
    use super::shape_key;

    /// A granted spawn's own directory, a workspace root and the proxy's ports
    /// change every spawn; re-measuring for each would cost every command the
    /// probe's ~100 ms.
    #[test]
    fn spawn_specific_paths_and_ports_share_one_key() {
        let profile = |dir: &str, port: u16| {
            format!(
                "(allow file-read* (subpath \"{dir}\"))\n\
                 (allow network-outbound (remote tcp4 \"localhost:{port}\"))\n\
                 (allow network-outbound (subpath \"{dir}/sock\"))\n"
            )
        };
        assert_eq!(
            shape_key(&profile("/Users/a/.harn/grant-spawns/1", 6000)),
            shape_key(&profile("/private/tmp/w/grant-spawns/2", 61234))
        );
    }

    /// The quirk depends on which rules come first, so a profile that gains or
    /// loses a path rule, or swaps a port for a wildcard, is measured again.
    #[test]
    fn rule_structure_changes_the_key() {
        let without_path = "(allow network-outbound (remote tcp4 \"localhost:6000\"))\n";
        let with_path = "(allow network-outbound (subpath \"/s\"))\n\
                         (allow network-outbound (remote tcp4 \"localhost:6000\"))\n";
        let wildcard = "(allow network-outbound (remote tcp4 \"localhost:*\"))\n";
        assert_ne!(shape_key(without_path), shape_key(with_path));
        assert_ne!(shape_key(without_path), shape_key(wildcard));
    }
}
