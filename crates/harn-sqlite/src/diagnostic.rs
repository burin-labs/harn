//! Safe facts for diagnosing accounting storage without rendering source data.
use std::error::Error;
use std::fmt;
use std::io::ErrorKind;

/// Typed codes only: no error messages, paths, SQL or database contents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailureDiagnostic {
    pub stage: &'static str,
    pub sqlite_primary_code: Option<i32>,
    pub sqlite_extended_code: Option<i32>,
    pub io_kind: Option<ErrorKind>,
    pub os_code: Option<i32>,
}

/// Follow typed error sources without inspecting or rendering their messages.
///
/// `stage` must be a caller-owned static operation name. A SQLite CANTOPEN code
/// describes an open failure, not proof of an operating-system access denial.
#[must_use]
pub fn failure_diagnostic(stage: &'static str, error: &(dyn Error + 'static)) -> FailureDiagnostic {
    let mut result = FailureDiagnostic {
        stage,
        sqlite_primary_code: None,
        sqlite_extended_code: None,
        io_kind: None,
        os_code: None,
    };
    let mut current = Some(error);
    // Error is an open trait; a malformed source chain must not hang reporting.
    for _ in 0..16 {
        let Some(error) = current else { break };
        if let Some(rusqlite::Error::SqliteFailure(code, _)) =
            error.downcast_ref::<rusqlite::Error>()
        {
            result
                .sqlite_primary_code
                .get_or_insert(code.extended_code & 0xff);
            result
                .sqlite_extended_code
                .get_or_insert(code.extended_code);
        }
        if let Some(error) = error.downcast_ref::<std::io::Error>() {
            result.io_kind.get_or_insert(error.kind());
            if let Some(code) = error.raw_os_error() {
                result.os_code.get_or_insert(code);
            }
        }
        current = error.source();
    }
    result
}

impl fmt::Display for FailureDiagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "stage={}", self.stage)?;
        if let Some(code) = self.sqlite_primary_code {
            write!(f, " sqlite_primary_code={code}")?;
        }
        if let Some(code) = self.sqlite_extended_code {
            write!(f, " sqlite_extended_code={code}")?;
        }
        if let Some(kind) = self.io_kind {
            write!(f, " io_kind={kind:?}")?;
        }
        if let Some(code) = self.os_code {
            write!(f, " os_code={code}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InitializationError;

    #[test]
    fn sqlite_codes_remain_distinct_and_do_not_expose_messages() {
        for code in [
            rusqlite::ffi::SQLITE_CANTOPEN,
            rusqlite::ffi::SQLITE_BUSY_RECOVERY,
            rusqlite::ffi::SQLITE_READONLY_DIRECTORY,
        ] {
            let error = rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(code),
                Some("fixture-secret-path-and-sql-value".into()),
            );
            let diagnostic = failure_diagnostic("open", &error);
            assert_eq!(diagnostic.sqlite_primary_code, Some(code & 0xff));
            assert_eq!(diagnostic.sqlite_extended_code, Some(code));
            assert_eq!(
                diagnostic.io_kind, None,
                "SQLite codes alone do not prove an OS denial"
            );
            assert!(!diagnostic.to_string().contains("fixture-secret"));
        }
    }

    #[test]
    fn initialization_preserves_stage_and_underlying_io_kind_without_paths() {
        let error: InitializationError<rusqlite::Error> =
            InitializationError::InitializationLockOpen {
                path: "/fixture-secret-directory/spend.sqlite.harn-init.lock".into(),
                source: std::io::Error::new(ErrorKind::PermissionDenied, "fixture-secret"),
            };
        let diagnostic = failure_diagnostic(error.stage(), &error);
        assert_eq!(diagnostic.stage, "initialization_lock_open");
        assert_eq!(diagnostic.io_kind, Some(ErrorKind::PermissionDenied));
        assert_eq!(diagnostic.sqlite_extended_code, None);
        assert!(!diagnostic.to_string().contains("fixture-secret"));
    }
}
