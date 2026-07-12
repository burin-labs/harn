use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Maximum supported rows or columns for one terminal.
pub const MAX_DIMENSION: u16 = 512;
/// Maximum number of detailed cells returned by one capture.
pub const MAX_CAPTURE_CELLS: usize = 16_384;
/// Maximum encoded input accepted by one send operation.
pub const MAX_INPUT_BYTES: usize = 65_536;
/// Default bounded raw-output history retained for diagnostics.
pub const DEFAULT_RAW_CAPACITY: usize = 256 * 1024;

/// Options used to start one terminal session.
#[derive(Clone, Debug)]
pub struct SessionOptions {
    /// Program and arguments. The first item is the executable.
    pub argv: Vec<String>,
    /// Initial number of terminal rows.
    pub rows: u16,
    /// Initial number of terminal columns.
    pub cols: u16,
    /// Child working directory.
    pub cwd: Option<PathBuf>,
    /// Explicit environment overlay. The caller owns inherited-env policy.
    pub env: BTreeMap<String, String>,
    /// Inherited environment keys to remove before applying `env`.
    pub env_remove: Vec<String>,
    /// Maximum raw-output bytes retained in memory.
    pub raw_capacity: usize,
}

impl SessionOptions {
    pub(crate) fn validate(&self) -> Result<(), TerminalError> {
        if self.argv.first().is_none_or(String::is_empty) {
            return Err(TerminalError::InvalidArgument(
                "argv must start with a non-empty executable".to_string(),
            ));
        }
        validate_dimensions(self.rows, self.cols)?;
        if self.raw_capacity == 0 {
            return Err(TerminalError::InvalidArgument(
                "raw_capacity must be greater than zero".to_string(),
            ));
        }
        if self.env_remove.iter().any(String::is_empty) {
            return Err(TerminalError::InvalidArgument(
                "env_remove entries must not be empty".to_string(),
            ));
        }
        Ok(())
    }
}

/// Current child-process state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ProcessStatus {
    /// The child is still running.
    Running,
    /// The child exited or was terminated by a signal.
    Exited {
        /// Numeric process exit code, when available.
        code: Option<u32>,
        /// Platform signal name, when available.
        signal: Option<String>,
    },
    /// Waiting for the child failed.
    Failed {
        /// Stable diagnostic explaining the wait failure.
        message: String,
    },
}

impl ProcessStatus {
    /// Whether the process has reached a terminal state.
    pub fn finished(&self) -> bool {
        !matches!(self, Self::Running)
    }
}

/// Optional bounded rectangle for detailed cell capture.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CellRegion {
    /// Zero-based first row.
    pub row: u16,
    /// Zero-based first column.
    pub column: u16,
    /// Number of rows to capture.
    pub rows: u16,
    /// Number of columns to capture.
    pub columns: u16,
}

impl CellRegion {
    pub(crate) fn validate(self, screen_rows: u16, screen_cols: u16) -> Result<(), TerminalError> {
        if self.rows == 0 || self.columns == 0 {
            return Err(TerminalError::InvalidArgument(
                "cell region rows and columns must be greater than zero".to_string(),
            ));
        }
        let end_row = self
            .row
            .checked_add(self.rows)
            .ok_or_else(|| TerminalError::InvalidArgument("cell row range overflowed".into()))?;
        let end_col = self
            .column
            .checked_add(self.columns)
            .ok_or_else(|| TerminalError::InvalidArgument("cell column range overflowed".into()))?;
        if end_row > screen_rows || end_col > screen_cols {
            return Err(TerminalError::InvalidArgument(format!(
                "cell region ({}, {}) {}x{} exceeds screen {}x{}",
                self.row, self.column, self.rows, self.columns, screen_rows, screen_cols
            )));
        }
        let count = usize::from(self.rows) * usize::from(self.columns);
        if count > MAX_CAPTURE_CELLS {
            return Err(TerminalError::InvalidArgument(format!(
                "cell region contains {count} cells; maximum is {MAX_CAPTURE_CELLS}"
            )));
        }
        Ok(())
    }
}

/// Terminal color attached to a captured cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TerminalColor {
    /// Terminal default color.
    Default,
    /// Indexed terminal palette color.
    Indexed {
        /// Palette index.
        index: u8,
    },
    /// Explicit RGB color.
    Rgb {
        /// Red channel.
        red: u8,
        /// Green channel.
        green: u8,
        /// Blue channel.
        blue: u8,
    },
}

impl From<vt100::Color> for TerminalColor {
    fn from(value: vt100::Color) -> Self {
        match value {
            vt100::Color::Default => Self::Default,
            vt100::Color::Idx(index) => Self::Indexed { index },
            vt100::Color::Rgb(red, green, blue) => Self::Rgb { red, green, blue },
        }
    }
}

/// One typed cell from a requested capture rectangle.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CellCapture {
    /// Zero-based row.
    pub row: u16,
    /// Zero-based column.
    pub column: u16,
    /// Cell text, including combining characters when present.
    pub text: String,
    /// Foreground color.
    pub foreground: TerminalColor,
    /// Background color.
    pub background: TerminalColor,
    /// Bold attribute.
    pub bold: bool,
    /// Italic attribute.
    pub italic: bool,
    /// Underline attribute.
    pub underline: bool,
    /// Inverse-video attribute.
    pub inverse: bool,
    /// Whether this cell contains a wide character.
    pub wide: bool,
    /// Whether this cell is the continuation half of a wide character.
    pub wide_continuation: bool,
}

/// Typed snapshot of the current visible screen and child state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ScreenCapture {
    /// Contract version for forwards-compatible consumers.
    pub schema_version: u32,
    /// Screen row count.
    pub rows: u16,
    /// Screen column count.
    pub columns: u16,
    /// Visible rows with terminal escapes resolved.
    pub text_rows: Vec<String>,
    /// Cursor row.
    pub cursor_row: u16,
    /// Cursor column.
    pub cursor_column: u16,
    /// Whether the cursor is visible.
    pub cursor_visible: bool,
    /// Whether the alternate screen buffer is active.
    pub alternate_screen: bool,
    /// Monotonic state revision.
    pub revision: u64,
    /// Total bytes received from the child.
    pub bytes_received: u64,
    /// Whether the bounded raw history discarded older bytes.
    pub raw_truncated: bool,
    /// VT parser error count.
    pub parser_errors: usize,
    /// Reader failure, if the PTY reader ended with an error.
    pub reader_error: Option<String>,
    /// Child process status.
    pub status: ProcessStatus,
    /// Detailed cells when a region was requested.
    pub cells: Option<Vec<CellCapture>>,
}

/// Result returned once a session has been quiet for the requested interval.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct WaitIdleResult {
    /// Revision observed at the idle boundary.
    pub revision: u64,
    /// Quiet duration requested by the caller.
    pub quiet_ms: u64,
    /// Child status at the idle boundary.
    pub status: ProcessStatus,
}

/// Errors returned by terminal-session operations.
#[derive(Debug, thiserror::Error)]
pub enum TerminalError {
    /// A caller-provided argument is invalid.
    #[error("invalid terminal-session argument: {0}")]
    InvalidArgument(String),
    /// The platform could not allocate a PTY.
    #[error("failed to allocate pseudo-terminal: {0}")]
    OpenPty(String),
    /// The child could not be spawned.
    #[error("failed to spawn terminal child: {0}")]
    Spawn(String),
    /// Terminal input or resize I/O failed.
    #[error("terminal I/O failed: {0}")]
    Io(String),
    /// A session operation was attempted after its terminal handles closed.
    #[error("terminal session is closed")]
    Closed,
    /// A bounded wait expired.
    #[error("terminal session timed out after {timeout_ms}ms while waiting for {operation}")]
    Timeout {
        /// Wait operation label.
        operation: &'static str,
        /// Configured timeout in milliseconds.
        timeout_ms: u64,
    },
    /// Internal synchronized state was poisoned by a panic.
    #[error("terminal session state was poisoned")]
    Poisoned,
}

pub(crate) fn validate_dimensions(rows: u16, cols: u16) -> Result<(), TerminalError> {
    if rows == 0 || cols == 0 {
        return Err(TerminalError::InvalidArgument(
            "rows and columns must be greater than zero".to_string(),
        ));
    }
    if rows > MAX_DIMENSION || cols > MAX_DIMENSION {
        return Err(TerminalError::InvalidArgument(format!(
            "rows and columns must not exceed {MAX_DIMENSION}"
        )));
    }
    Ok(())
}
