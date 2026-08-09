use super::{DictMap, ErrorCategory, VmDictExt, VmError, VmValue};

/// Canonical, stable string key for an [`std::io::ErrorKind`].
///
/// These keys are a script-facing contract. Keep the mapping centralized so
/// filesystem and process boundaries never drift into different spellings.
pub fn io_error_kind_str(error: &std::io::Error) -> &'static str {
    use std::io::ErrorKind;
    match error.kind() {
        ErrorKind::NotFound => "not_found",
        ErrorKind::PermissionDenied => "permission_denied",
        ErrorKind::AlreadyExists => "already_exists",
        ErrorKind::StorageFull => "storage_full",
        ErrorKind::QuotaExceeded => "quota_exceeded",
        ErrorKind::FileTooLarge => "file_too_large",
        ErrorKind::ReadOnlyFilesystem => "read_only_filesystem",
        ErrorKind::NotADirectory => "not_a_directory",
        ErrorKind::IsADirectory => "is_a_directory",
        ErrorKind::DirectoryNotEmpty => "directory_not_empty",
        ErrorKind::CrossesDevices => "crosses_devices",
        ErrorKind::TooManyLinks => "too_many_links",
        ErrorKind::InvalidInput => "invalid_input",
        ErrorKind::InvalidData => "invalid_data",
        ErrorKind::TimedOut => "timed_out",
        ErrorKind::Interrupted => "interrupted",
        ErrorKind::UnexpectedEof => "unexpected_eof",
        ErrorKind::WouldBlock => "would_block",
        ErrorKind::OutOfMemory => "out_of_memory",
        ErrorKind::ResourceBusy => "resource_busy",
        ErrorKind::ExecutableFileBusy => "executable_file_busy",
        // `ErrorKind` is non-exhaustive. Unknown platform kinds deliberately
        // collapse to one stable value instead of leaking a debug spelling.
        _ => "other",
    }
}

/// Lower an OS I/O failure to the stable script-facing value.
pub fn io_error_value(
    error: &std::io::Error,
    message: impl AsRef<str>,
    category: Option<ErrorCategory>,
) -> VmValue {
    let mut failure = DictMap::new();
    failure.put_str("error", "io_error");
    failure.put_str("kind", io_error_kind_str(error));
    failure.put_str("message", message);
    if let Some(category) = category {
        failure.put_str("category", category.as_str());
    }
    VmValue::dict(failure)
}

/// Wrap [`io_error_value`] for builtins that throw instead of returning it.
pub fn io_error_thrown(
    error: &std::io::Error,
    message: impl AsRef<str>,
    category: Option<ErrorCategory>,
) -> VmError {
    VmError::Thrown(io_error_value(error, message, category))
}

/// Throw an I/O failure classified as an environmental runtime condition.
pub fn environment_io_error_thrown(error: &std::io::Error, message: impl AsRef<str>) -> VmError {
    io_error_thrown(error, message, Some(ErrorCategory::Environment))
}
