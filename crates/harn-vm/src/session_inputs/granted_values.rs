//! Exact-value redaction for credentials an environment grant handed to a child.
//!
//! A grant exposes a value to a process. Nothing stops that process from
//! printing it (`gh auth token`, a verbose HTTP log, a test that echoes its
//! environment), and process output becomes tool results, events, and
//! transcripts. Every secret value a grant exposes is registered here when it
//! is handed to a child, and [`super::OutputFilter`] removes it, with its
//! common encodings, from everything that filter covers.
//!
//! The set is process-wide and only grows, like the retired-secret set of a
//! session input store: a value may still be in flight in output after the
//! session that granted it ends. Short values are not registered, because
//! replacing a common short string would corrupt ordinary output.

use base64::{engine::general_purpose::STANDARD, Engine as _};
use parking_lot::Mutex;

/// Below this a value is too likely to occur in unrelated output.
const MIN_VALUE_LEN: usize = 8;
/// A bound on the registry, so a runaway caller cannot make every output
/// scan arbitrarily slow. Past it, new values are still filtered: the oldest
/// registration is never dropped, the newest refuses to register, and
/// [`register_granted_value`] reports that so the caller can fail closed.
const MAX_FORMS: usize = 1024;

static FORMS: Mutex<Vec<Vec<u8>>> = Mutex::new(Vec::new());

/// The forms a value can take in output: itself, standard base64 of itself
/// and of the `x-access-token:VALUE` basic-auth pair git and gh send, and
/// its percent-encoding.
fn forms(value: &str) -> Vec<Vec<u8>> {
    let mut forms = vec![
        value.as_bytes().to_vec(),
        STANDARD.encode(value).into_bytes(),
        STANDARD
            .encode(format!("x-access-token:{value}"))
            .into_bytes(),
        percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC)
            .to_string()
            .into_bytes(),
    ];
    // Base64 padding depends on what follows the value in a larger encoded
    // string; match the unpadded body too.
    let unpadded: Vec<Vec<u8>> = forms[1..3]
        .iter()
        .map(|form| {
            let end = form
                .iter()
                .rposition(|byte| *byte != b'=')
                .map_or(0, |i| i + 1);
            form[..end].to_vec()
        })
        .collect();
    forms.extend(unpadded);
    forms.retain(|form| form.len() >= MIN_VALUE_LEN);
    forms.sort();
    forms.dedup();
    forms
}

/// Register a value a grant is about to hand to a child. Returns `false`
/// only when the registry is full and the value could not be added, so the
/// caller must not expose it.
#[must_use]
pub fn register_granted_value(value: &str) -> bool {
    if value.len() < MIN_VALUE_LEN {
        return true;
    }
    let mut registered = FORMS.lock();
    let new: Vec<Vec<u8>> = forms(value)
        .into_iter()
        .filter(|form| !registered.contains(form))
        .collect();
    if registered.len() + new.len() > MAX_FORMS {
        return false;
    }
    registered.extend(new);
    // Longest first, so a value never leaves a fragment of a longer form.
    registered.sort_unstable_by(|left, right| right.len().cmp(&left.len()).then(left.cmp(right)));
    true
}

pub(super) fn any() -> bool {
    !FORMS.lock().is_empty()
}

/// `bytes` with every registered form replaced, or `None` when none occurs.
pub(super) fn redact(bytes: &[u8]) -> Option<Vec<u8>> {
    let forms = FORMS.lock();
    if forms.is_empty()
        || !forms
            .iter()
            .any(|form| memchr::memmem::find(bytes, form).is_some())
    {
        return None;
    }
    let mut text = bytes.to_vec();
    for form in forms.iter() {
        let mut out = Vec::with_capacity(text.len());
        let mut rest = text.as_slice();
        while let Some(start) = memchr::memmem::find(rest, form) {
            out.extend_from_slice(&rest[..start]);
            out.extend_from_slice(crate::redact::REDACTED_PLACEHOLDER.as_bytes());
            rest = &rest[start + form.len()..];
        }
        out.extend_from_slice(rest);
        text = out;
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_registered_form_is_redacted() {
        let value = "probe-granted-values-0101";
        assert!(register_granted_value(value));
        let basic = STANDARD.encode(format!("x-access-token:{value}"));
        let text = format!(
            "plain={value} b64={} basic={basic} header=Basic {} url={}",
            STANDARD.encode(value),
            basic.trim_end_matches('='),
            "probe%2Dgranted%2Dvalues%2D0101",
        );
        let redacted = String::from_utf8(redact(text.as_bytes()).unwrap()).unwrap();
        assert!(!redacted.contains(value), "{redacted}");
        assert!(!redacted.contains(&STANDARD.encode(value)), "{redacted}");
        assert!(
            !redacted.contains(basic.trim_end_matches('=')),
            "{redacted}"
        );
        assert!(!redacted.contains("probe%2D"), "{redacted}");
        assert!(redacted.starts_with("plain=[redacted]"), "{redacted}");
    }

    #[test]
    fn short_values_and_unrelated_output_are_untouched() {
        assert!(register_granted_value("short"));
        assert_eq!(redact(b"a short line"), None);
    }
}
