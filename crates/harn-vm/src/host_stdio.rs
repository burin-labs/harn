//! One place the runtime's diagnostic writes go when a host owns the terminal.
//!
//! When the runtime is linked in-process, file descriptors 1 and 2 belong to
//! the embedder. A host drawing a full-screen interface cannot survive a write
//! to them: the bytes land in the live cell grid wherever the cursor happens to
//! be, and a renderer that diffs its own frame buffers emits only the cells it
//! believes changed, so a write it never saw is never repainted. The damage
//! persists until that region changes for an unrelated reason.
//!
//! A host can defend itself by capturing the descriptor, and one has, but that
//! is the wrong layer. It makes every embedder know that the runtime writes to
//! descriptors, and it captures unrelated writes from the rest of the process
//! along with the runtime's. The runtime knows which writes are its own.
//!
//! So a host installs one sink at session start and receives those writes as
//! data, with the stream named so it can label the source when it forwards. A
//! host that installs nothing keeps the descriptor, which is why an ordinary
//! command-line embedding is unchanged by this.
//!
//! ## What is routed here, and what is deliberately not
//!
//! Routed: the harness stdio facade, which is what guest code calls, and the
//! runtime's own log exporter.
//!
//! Not routed, because their stdout is the channel rather than a diagnostic:
//! the language-server, debug-adapter, agent-protocol and model-context
//! transports, and the merge-captain driver's opt-in event stream, which a
//! caller asks for by name. A sink that swallowed those would break the
//! protocol rather than protect the screen.
//!
//! Not routed, because the process owns them: the command-line front end's own
//! output, and the supervisor's startup handshake, whose stderr is a pipe its
//! parent reads rather than anybody's terminal.

use std::sync::{Arc, RwLock};

/// Which of the process's two output streams a write was headed for.
///
/// Carried rather than collapsed, because a host that forwards these needs to
/// label the source, and because a host may route them differently: a warning
/// belongs in a status area, and program output usually does not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostStdioStream {
    Stdout,
    Stderr,
}

impl HostStdioStream {
    /// The stream's conventional name, for a host that labels what it forwards.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        }
    }
}

/// A host's destination for runtime diagnostic writes.
///
/// `text` arrives exactly as the runtime produced it, including any trailing
/// newline, and is not guaranteed to be a whole line: a guest that prints
/// without a newline produces a partial one, and joining them is the host's
/// business because only the host knows what it is joining them into.
pub trait HostStdioSink: Send + Sync {
    fn write(&self, stream: HostStdioStream, text: &str);
}

static HOST_STDIO_SINK: RwLock<Option<Arc<dyn HostStdioSink>>> = RwLock::new(None);

/// Install the sink for this process, returning whatever it replaced.
///
/// Process-global rather than per-thread on purpose. A session's work crosses
/// threads, and a sink that covered only the installing thread would leave the
/// writes a host most wants to catch still reaching the descriptor, while
/// looking installed.
pub fn install_host_stdio_sink(sink: Arc<dyn HostStdioSink>) -> Option<Arc<dyn HostStdioSink>> {
    match HOST_STDIO_SINK.write() {
        Ok(mut slot) => slot.replace(sink),
        Err(poisoned) => poisoned.into_inner().replace(sink),
    }
}

/// Give the descriptors back, returning the sink that was installed.
pub fn clear_host_stdio_sink() -> Option<Arc<dyn HostStdioSink>> {
    match HOST_STDIO_SINK.write() {
        Ok(mut slot) => slot.take(),
        Err(poisoned) => poisoned.into_inner().take(),
    }
}

/// Whether a host currently owns the runtime's diagnostic writes.
#[must_use]
pub fn host_stdio_sink_installed() -> bool {
    current().is_some()
}

fn current() -> Option<Arc<dyn HostStdioSink>> {
    match HOST_STDIO_SINK.read() {
        Ok(slot) => slot.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

/// Offer one write to the host's sink.
///
/// Returns whether the sink took it. A `false` means no sink is installed and
/// the caller must fall through to the descriptor, which is the default this
/// preserves for every embedding that installs nothing.
///
/// The lock is released before the sink runs, so a sink that writes through
/// the runtime again cannot deadlock against its own installation.
pub(crate) fn emit(stream: HostStdioStream, text: &str) -> bool {
    match current() {
        Some(sink) => {
            sink.write(stream, text);
            true
        }
        None => false,
    }
}
