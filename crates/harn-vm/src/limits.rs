//! Shared allocation guards for script-controlled sizes.
//!
//! Several builtins and operators repeat a string by a count that comes
//! straight from a Harn program (`"a" * n`, `s.repeat(n)`, `str_pad`,
//! `pad_left`/`pad_right`). Without a cap, a large count makes `String::repeat`
//! either exhaust memory or panic with `capacity overflow` — a script should
//! never be able to crash the host that way. These helpers centralise the cap
//! so every site shares one limit and one error.

use crate::value::VmError;

/// Upper bound on the byte length a single repeat/pad expansion may produce
/// (16 MiB). A typo (`"-" * 1_000_000_000`) errors instead of allocating
/// gigabytes or panicking.
pub(crate) const MAX_REPEAT_OUTPUT_BYTES: usize = 1 << 24;

/// Repeat `s` `count` times, refusing to build a string larger than
/// [`MAX_REPEAT_OUTPUT_BYTES`]. Returns an empty string for non-positive
/// counts (callers pass `count.max(0)`), and a clean [`VmError::Runtime`]
/// rather than panicking/OOM-ing when the result would be too large.
pub(crate) fn checked_repeat(s: &str, count: usize) -> Result<String, VmError> {
    let total = s.len().saturating_mul(count);
    if total > MAX_REPEAT_OUTPUT_BYTES {
        return Err(VmError::Runtime(format!(
            "repeat: output would be {total} bytes (limit {MAX_REPEAT_OUTPUT_BYTES})"
        )));
    }
    Ok(s.repeat(count))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_repeat_ok() {
        assert_eq!(checked_repeat("ab", 3).unwrap(), "ababab");
        assert_eq!(checked_repeat("x", 0).unwrap(), "");
    }

    #[test]
    fn oversized_repeat_errors_without_panic() {
        // Would be ~9.2e18 bytes — must error, never allocate or panic.
        let err = checked_repeat("ab", usize::MAX / 2).unwrap_err();
        assert!(matches!(err, VmError::Runtime(_)));
        // Just under vs over the cap.
        assert!(checked_repeat("a", MAX_REPEAT_OUTPUT_BYTES).is_ok());
        assert!(checked_repeat("a", MAX_REPEAT_OUTPUT_BYTES + 1).is_err());
    }
}
