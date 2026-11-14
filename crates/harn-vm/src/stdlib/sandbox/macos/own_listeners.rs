//! Which local listeners a command behind the managed proxy may connect to.
//!
//! A loopback grant exists so a command can run a local server and connect to
//! it: test suites, dev servers, browsers driving a dev server. Seatbelt names
//! a destination only as `localhost:<port>` and cannot tell whose listener is
//! on the other end, so the profile is computed at spawn instead. It admits
//! every local port, then denies each port that some other process's listener
//! holds at that moment, then re-admits the proxy's ports. A listener counts as
//! this run's own when its process carries this runtime's lineage marker
//! ([`super::super::process_output::CHILD_LINEAGE_ENV`]), which every child
//! behind the proxy inherits; a dev server that an earlier command left running
//! keeps it.
//!
//! The socket table is the kernel's, for every user: `net.inet.tcp.pcblist64`
//! and `net.inet.udp.pcblist64`. Owners come from `libproc` for this user's
//! processes. Both readers check themselves on every scan against sockets this
//! process just opened, and the rule shape is checked once per process under a
//! real profile. When either check fails, the command gets the strict
//! proxy-only profile instead.
//!
//! What this leaves open: a service that starts listening after the command
//! starts is not in the command's profile, which Seatbelt fixes at exec.

use std::collections::BTreeSet;
use std::net::{TcpListener, UdpSocket};

use crate::orchestration::CapabilityPolicy;

/// Profile rules for a loopback grant behind the proxy, appended after the
/// strict proxy-only rules. `Err` names why they could not be trusted.
pub(super) fn rules(policy: &CapabilityPolicy) -> Option<Result<String, String>> {
    let grant = super::super::policy::ProcessNetworkGrant::for_policy(policy);
    let proxy = policy
        .process_network_proxy
        .filter(|_| grant.loopback_behind_proxy())?;
    let proxy_ports = [proxy.http_port, proxy.socks_port];
    Some(
        shape_holds()
            .and_then(|()| foreign_listeners())
            .map(|foreign| render(foreign.tcp.union(&foreign.udp).copied(), proxy_ports)),
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

/// Whether [`render`]'s rules deny what they name on this host, measured once
/// per process with `nc` under a real profile: a denied listener, the proxy's
/// port over IPv4 and over IPv6, and a listener the rules leave open.
fn shape_holds() -> Result<(), String> {
    static MEASURED: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
    MEASURED.get_or_init(measure_shape).clone()
}

fn measure_shape() -> Result<(), String> {
    let listen = |address: &str| {
        TcpListener::bind(address).map_err(|error| format!("shape probe listener: {error}"))
    };
    let port = |listener: &TcpListener| listener.local_addr().map(|a| a.port()).unwrap_or(0);
    let (denied, open) = (listen("127.0.0.1:0")?, listen("127.0.0.1:0")?);
    let proxy = listen("127.0.0.1:0")?;
    let proxy_v6 = listen(&format!("[::1]:{}", port(&proxy)))?;
    let profile = format!(
        "(version 1)\n(allow default)\n(deny network-outbound)\n{}",
        render([port(&denied)], [port(&proxy); 2])
    );
    let connects = |host: &str, port: u16| {
        std::process::Command::new(super::SANDBOX_EXEC_PATH)
            .args([
                "-p",
                &profile,
                "/usr/bin/nc",
                "-z",
                "-G",
                "2",
                "-w",
                "2",
                host,
            ])
            .arg(port.to_string())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|status| status.success())
            .map_err(|error| format!("shape probe: {error}"))
    };
    let measured = [
        (
            "a denied listener",
            connects("127.0.0.1", port(&denied))?,
            false,
        ),
        (
            "the proxy over IPv4",
            connects("127.0.0.1", port(&proxy))?,
            true,
        ),
        (
            "the proxy port over IPv6",
            connects("::1", port(&proxy_v6))?,
            false,
        ),
        (
            "an undenied listener",
            connects("127.0.0.1", port(&open))?,
            true,
        ),
    ];
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

#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Ports {
    pub(super) tcp: BTreeSet<u16>,
    pub(super) udp: BTreeSet<u16>,
}

/// Ports that some process other than this run's children listens on now.
pub(super) fn foreign_listeners() -> Result<Ports, String> {
    // Known sockets every reader must find, or the readers are wrong.
    let probe_tcp = TcpListener::bind("127.0.0.1:0").map_err(|e| format!("probe listener: {e}"))?;
    let probe_udp = UdpSocket::bind("127.0.0.1:0").map_err(|e| format!("probe socket: {e}"))?;
    let tcp_port = probe_tcp.local_addr().map_err(|e| e.to_string())?.port();
    let udp_port = probe_udp.local_addr().map_err(|e| e.to_string())?.port();
    let me = unsafe { libc::getuid() };

    let tcp = table::listeners(table::Protocol::Tcp)?;
    let udp = table::listeners(table::Protocol::Udp)?;
    if !tcp.contains(&(tcp_port, me)) || !udp.contains(&(udp_port, me)) {
        return Err("the kernel socket table did not list this process's own sockets".into());
    }
    let own_process = owners::sockets(std::process::id() as i32)?;
    if !own_process.tcp.contains(&tcp_port) || !own_process.udp.contains(&udp_port) {
        return Err("libproc did not list this process's own sockets".into());
    }

    let (ours, theirs) = owners::by_lineage(me)?;
    // A port is this run's only when a marked process holds it and no other
    // process does: Seatbelt matches the port at every local address, so a
    // shared port number would admit the other listener too.
    let foreign =
        |table: &BTreeSet<(u16, u32)>, ours: &BTreeSet<u16>, theirs: &BTreeSet<u16>, probe: u16| {
            table
                .iter()
                .filter(|(port, uid)| *uid != me || !ours.contains(port) || theirs.contains(port))
                .map(|(port, _)| *port)
                // The probe closes before the command starts.
                .filter(|port| *port != probe)
                .collect()
        };
    Ok(Ports {
        tcp: foreign(&tcp, &ours.tcp, &theirs.tcp, tcp_port),
        udp: foreign(&udp, &ours.udp, &theirs.udp, udp_port),
    })
}

/// The kernel's socket table, through the public `pcblist64` records.
mod table {
    use std::collections::BTreeSet;

    pub(super) enum Protocol {
        Tcp,
        Udp,
    }

    // Offsets in the packed SDK records (`netinet/tcp_var.h`, `in_pcb.h`,
    // `sys/socketvar.h`); the self-check in `foreign_listeners` vouches for them.
    const XINPGEN_LEN: usize = 24;
    const INPCB_LPORT: usize = 18;
    const INPCB_UID: usize = 144 + 104;
    const TCPCB_INPCB: usize = 4;
    const TCPCB_STATE: usize = 292;
    const TCPS_LISTEN: i32 = 1;

    /// `(local port, owner uid)` of every TCP listener or bound UDP socket.
    pub(super) fn listeners(protocol: Protocol) -> Result<BTreeSet<(u16, u32)>, String> {
        let name = match protocol {
            Protocol::Tcp => c"net.inet.tcp.pcblist64",
            Protocol::Udp => c"net.inet.udp.pcblist64",
        };
        let buffer = read_sysctl(name)?;
        let u32_at = |at: usize| -> Option<u32> {
            Some(u32::from_ne_bytes(buffer.get(at..at + 4)?.try_into().ok()?))
        };
        let port_at = |at: usize| -> Option<u16> {
            Some(u16::from_be_bytes(buffer.get(at..at + 2)?.try_into().ok()?))
        };
        let mut found = BTreeSet::new();
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
                Protocol::Tcp => {
                    u32_at(at + TCPCB_STATE).ok_or("truncated TCP record")? as i32 == TCPS_LISTEN
                }
                Protocol::Udp => true,
            };
            let port = port_at(inpcb + INPCB_LPORT).ok_or("truncated socket record")?;
            let uid = u32_at(inpcb + INPCB_UID).ok_or("truncated socket record")?;
            if listening && port != 0 {
                found.insert((port, uid));
            }
            at += len;
        }
        Ok(found)
    }

    fn read_sysctl(name: &std::ffi::CStr) -> Result<Vec<u8>, String> {
        // The table can grow between the size query and the read; retry once
        // with room to spare before giving up.
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

/// Which local ports this user's processes hold, through `libproc`.
mod owners {
    use super::Ports;

    // `sys/proc_info.h`: `struct socket_fdinfo` and the parts of
    // `struct socket_info` read here.
    const SOCKET_FDINFO_LEN: usize = 792;
    const PSI: usize = 24;
    const SOI_KIND: usize = PSI + 232;
    const SOI_PROTO: usize = PSI + 240;
    const INSI_LPORT: usize = 4;
    const TCPSI_STATE: usize = 80;
    const SOCKINFO_IN: i32 = 1;
    const SOCKINFO_TCP: i32 = 2;
    const TSI_S_LISTEN: i32 = 1;
    const PROX_FDTYPE_SOCKET: u32 = 2;
    const PROC_PIDFDSOCKETINFO: i32 = 3;

    /// Listening TCP and bound UDP ports of `uid`'s processes, split into
    /// those that carry this runtime's lineage marker and those that do not.
    pub(super) fn by_lineage(uid: u32) -> Result<(Ports, Ports), String> {
        let marker = format!(
            "{}={}\0",
            crate::stdlib::sandbox::process_output::CHILD_LINEAGE_ENV,
            crate::stdlib::sandbox::process_output::child_lineage()
        );
        let (mut ours, mut theirs) = (Ports::default(), Ports::default());
        let mut args = Vec::new();
        for pid in all_pids()? {
            if pid_uid(pid) != Some(uid) {
                continue;
            }
            // A process that exited since the listing holds nothing.
            let Ok(ports) = sockets(pid) else { continue };
            if ports.tcp.is_empty() && ports.udp.is_empty() {
                continue;
            }
            let side = if carries(pid, marker.as_bytes(), &mut args) {
                &mut ours
            } else {
                &mut theirs
            };
            side.tcp.extend(ports.tcp);
            side.udp.extend(ports.udp);
        }
        Ok((ours, theirs))
    }

    pub(super) fn sockets(pid: i32) -> Result<Ports, String> {
        let mut ports = Ports::default();
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
            if read as usize != SOCKET_FDINFO_LEN {
                continue;
            }
            let i32_at = |at: usize| i32::from_ne_bytes(info[at..at + 4].try_into().unwrap());
            let lport = |inp: usize| u16::from_be(i32_at(inp + INSI_LPORT) as u16);
            match i32_at(SOI_KIND) {
                SOCKINFO_TCP if i32_at(SOI_PROTO + TCPSI_STATE) == TSI_S_LISTEN => {
                    ports.tcp.insert(lport(SOI_PROTO));
                }
                SOCKINFO_IN => {
                    ports.udp.insert(lport(SOI_PROTO));
                }
                _ => {}
            }
        }
        ports.tcp.remove(&0);
        ports.udp.remove(&0);
        Ok(ports)
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
