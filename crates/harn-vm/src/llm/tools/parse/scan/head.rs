//! Leading-call-head extraction — the field pair that keeps the Harn walk off
//! the regex path.

use super::super::syntax::ident_length;
use super::unit::CallHead;

/// The leading call head of `text`: an identifier immediately followed, modulo
/// inline whitespace, by `(` or `{`.
///
/// `None` when `text` does not open that way — including when it opens with an
/// identifier that no separator follows, because a bare word is narration, not
/// a call head. Whether `name` is a registered tool is deliberately not
/// answered here; that is policy, and policy lives in Harn.
pub(crate) fn call_head(text: &str) -> Option<CallHead> {
    let trimmed = text.trim_start();
    let bytes = trimmed.as_bytes();
    let name_len = ident_length(bytes)?;
    let mut idx = name_len;
    while matches!(bytes.get(idx), Some(b' ') | Some(b'\t')) {
        idx += 1;
    }
    let sep = match bytes.get(idx) {
        Some(b'(') => '(',
        Some(b'{') => '{',
        _ => return None,
    };
    Some(CallHead {
        name: trimmed[..name_len].to_string(),
        sep,
    })
}
