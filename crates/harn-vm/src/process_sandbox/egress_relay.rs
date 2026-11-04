//! The way out of a child's private network namespace: its proxy ports.
//!
//! A child in a private network namespace has no route to the host proxy's
//! loopback ports, because the host's loopback is not its loopback. So the two
//! ends meet on a Unix socket, which belongs to the filesystem rather than to a
//! network namespace. The host bridges each socket to its proxy port. Inside the
//! namespace, a relay the namespace helper forks listens on the same port
//! numbers and forwards to those sockets. The child sees the proxy where
//! [`crate::orchestration::ProcessNetworkProxy`] says it is, and has nothing else
//! to connect to: no interface but loopback, and no listener but the relay.

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use crate::orchestration::ProcessNetworkProxy;

/// Flag naming what the namespace helper's relay forwards, and where to.
pub const NETNS_EGRESS_RELAY_FLAG: &str = "--egress-relay";

pub(crate) const HTTP_SOCKET: &str = "http.sock";
pub(crate) const SOCKS_SOCKET: &str = "socks.sock";

/// The proxy ports a namespace helper relays, and the directory holding the
/// host's Unix sockets. The argument contract exists on every CLI platform;
/// Linux executes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EgressRelayTarget {
    pub http_port: u16,
    pub socks_port: u16,
    pub directory: PathBuf,
}

impl EgressRelayTarget {
    /// Where the bridge for this proxy lives. Fixed once per process, because
    /// a child's closed environment carries its own `TMPDIR` and the bridge and
    /// the spawn must name one directory.
    #[cfg(target_os = "linux")]
    pub(crate) fn for_proxy(proxy: ProcessNetworkProxy) -> Self {
        static BASE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
        let base = BASE.get_or_init(std::env::temp_dir);
        let uid = unsafe { libc::getuid() };
        Self {
            http_port: proxy.http_port,
            socks_port: proxy.socks_port,
            directory: base.join(format!(
                "harn-egress-{uid}-{}-{}",
                proxy.http_port, proxy.socks_port
            )),
        }
    }

    #[cfg(not(target_os = "linux"))]
    #[allow(dead_code)]
    pub(crate) fn for_proxy(proxy: ProcessNetworkProxy) -> Self {
        Self {
            http_port: proxy.http_port,
            socks_port: proxy.socks_port,
            directory: PathBuf::new(),
        }
    }
}

impl fmt::Display for EgressRelayTarget {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            out,
            "{}:{}:{}",
            self.http_port,
            self.socks_port,
            self.directory.display()
        )
    }
}

impl FromStr for EgressRelayTarget {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        const SHAPE: &str = "egress relay requires HTTP-PORT:SOCKS-PORT:absolute-path";
        let mut parts = value.splitn(3, ':');
        let mut port = || {
            parts
                .next()
                .and_then(|text| text.parse::<u16>().ok())
                .filter(|port| *port != 0)
                .ok_or(SHAPE)
        };
        let http_port = port()?;
        let socks_port = port()?;
        let directory = PathBuf::from(parts.next().ok_or(SHAPE)?);
        if !directory.is_absolute() || value.contains('\0') {
            return Err(SHAPE.into());
        }
        Ok(Self {
            http_port,
            socks_port,
            directory,
        })
    }
}

/// A path to `name` through an open directory, so it stays short enough for a
/// socket address and stays right after the helper mounts over `/tmp`.
#[cfg(target_os = "linux")]
pub(crate) fn socket_through(directory: &std::fs::File, name: &str) -> PathBuf {
    use std::os::fd::AsRawFd;
    PathBuf::from(format!("/proc/self/fd/{}/{name}", directory.as_raw_fd()))
}

/// Carry one connection both ways, passing on each side's end of stream, so a
/// client that half-closes still gets its reply.
#[cfg(target_os = "linux")]
pub(crate) fn splice(
    unix: std::os::unix::net::UnixStream,
    tcp: std::net::TcpStream,
) -> std::io::Result<()> {
    use std::net::Shutdown;

    let mut unix_read = unix.try_clone()?;
    let mut tcp_write = tcp.try_clone()?;
    #[allow(clippy::disallowed_methods)] // One connection's relay, outside any run's scope.
    let forward = crate::runtime_stack::spawn(move || {
        let copied = std::io::copy(&mut unix_read, &mut tcp_write);
        let _ = tcp_write.shutdown(Shutdown::Write);
        copied
    });
    let mut tcp_read = tcp;
    let mut unix_write = unix;
    let reverse = std::io::copy(&mut tcp_read, &mut unix_write);
    let _ = unix_write.shutdown(Shutdown::Write);
    forward
        .join()
        .map_err(|_| std::io::Error::other("egress relay thread panicked"))??;
    reverse?;
    Ok(())
}

/// Bind the proxy's ports on the namespace's loopback and fork the relay that
/// serves them.
///
/// Call after loopback is up and before anything mounts over the directory: the
/// relay holds it open. A failure here refuses the launch, because a child with
/// no relay has no network at all and would read that as a broken host.
///
/// The relay holds no descriptor of the payload's, so it cannot keep a command's
/// output open past its exit. It enters the payload's confinement, and dies with
/// the process that execs the payload. A daemon that outlives its command loses
/// the proxy, keeping its loopback.
#[cfg(target_os = "linux")]
pub fn start_namespace_relay(
    target: &EgressRelayTarget,
    confinement: &crate::process_sandbox::TransferableConfinement,
) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;

    let directory = std::fs::File::open(&target.directory)?;
    let http = std::net::TcpListener::bind(("127.0.0.1", target.http_port))?;
    let socks = std::net::TcpListener::bind(("127.0.0.1", target.socks_port))?;
    let parent = unsafe { libc::getpid() };
    match unsafe { libc::fork() } {
        -1 => return Err(std::io::Error::last_os_error()),
        0 => {}
        _ => return Ok(()),
    }
    let keep = [directory.as_raw_fd(), http.as_raw_fd(), socks.as_raw_fd()];
    let status = match become_relay(parent, &keep, confinement) {
        Ok(()) => serve(http, socks, directory),
        Err(_) => 1,
    };
    unsafe { libc::_exit(status) }
}

#[cfg(target_os = "linux")]
fn become_relay(
    parent: libc::pid_t,
    keep: &[std::os::fd::RawFd],
    confinement: &crate::process_sandbox::TransferableConfinement,
) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;

    if unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // The parent may have died before that took effect.
    if unsafe { libc::getppid() } != parent {
        return Err(std::io::Error::other("namespace helper already exited"));
    }
    let null = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/null")?;
    for standard in 0..3 {
        if unsafe { libc::dup2(null.as_raw_fd(), standard) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    confinement.enter()?;
    let open = std::fs::read_dir("/proc/self/fd")
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|entry| entry.file_name().to_str()?.parse::<i32>().ok())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for fd in open {
        if fd > 2 && !keep.contains(&fd) {
            unsafe {
                libc::close(fd);
            }
        }
    }
    Ok(())
}

/// Serve both ports until the namespace's owner dies. Returns only on failure.
#[cfg(target_os = "linux")]
fn serve(
    http: std::net::TcpListener,
    socks: std::net::TcpListener,
    directory: std::fs::File,
) -> i32 {
    let directory = std::sync::Arc::new(directory);
    let socks_directory = std::sync::Arc::clone(&directory);
    #[allow(clippy::disallowed_methods)] // The relay process has no run scope.
    let _socks = crate::runtime_stack::spawn(move || {
        forward_each(&socks, &socks_directory, SOCKS_SOCKET);
    });
    forward_each(&http, &directory, HTTP_SOCKET);
    1
}

#[cfg(target_os = "linux")]
fn forward_each(
    listener: &std::net::TcpListener,
    directory: &std::sync::Arc<std::fs::File>,
    socket: &'static str,
) {
    while let Ok((client, _peer)) = listener.accept() {
        let directory = std::sync::Arc::clone(directory);
        #[allow(clippy::disallowed_methods)] // The relay process has no run scope.
        let _connection = crate::runtime_stack::spawn(move || {
            let host = std::os::unix::net::UnixStream::connect(socket_through(&directory, socket))?;
            splice(host, client)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_target_survives_the_helper_argument() {
        let target = EgressRelayTarget {
            http_port: 4100,
            socks_port: 4101,
            directory: PathBuf::from("/run/user/1000/with:colon"),
        };
        assert_eq!(target.to_string().parse(), Ok(target));
        for malformed in ["", "1:2", "0:2:/tmp", "1:2:relative", "x:2:/tmp"] {
            assert!(
                malformed.parse::<EgressRelayTarget>().is_err(),
                "{malformed}"
            );
        }
    }
}
