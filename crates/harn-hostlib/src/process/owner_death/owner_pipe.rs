use std::io;

pub(super) fn wait_for_owner_pipe_close() -> io::Result<()> {
    loop {
        let mut pipe = libc::pollfd {
            fd: libc::STDIN_FILENO,
            // Hangup is reported without requesting readable events, so
            // unconsumed payload bytes cannot make the watcher spin.
            events: 0,
            revents: 0,
        };
        if unsafe { libc::poll(&raw mut pipe, 1, -1) } < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if pipe.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
            return Ok(());
        }
    }
}
