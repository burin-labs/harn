//! Typed pseudo-terminal sessions for deterministic TUI driving.
//!
//! This crate owns the process/PTY/VT mechanics shared by Harn host
//! capabilities and product-level terminal smoke tests. It deliberately has
//! no VM or product dependency: callers supply an argv vector and receive a
//! typed screen capture rather than terminal escape bytes.

#![deny(rust_2018_idioms)]
#![warn(missing_docs)]

mod input;
mod session;
mod types;

pub use input::{InputEvent, KeyCode, Modifier, NamedKey};
pub use session::TerminalSession;
pub use types::{
    CellCapture, CellRegion, ProcessStatus, ScreenCapture, SessionOptions, TerminalColor,
    TerminalError, WaitIdleResult, DEFAULT_RAW_CAPACITY, MAX_CAPTURE_CELLS, MAX_DIMENSION,
    MAX_INPUT_BYTES,
};
