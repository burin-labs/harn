//! Tests for `owned<T>` ownership lints (HARN-OWN-003).
//!
//! `owned<T>` carries sole ownership of a drop-able stdlib handle: the compiler
//! emits an implicit `defer { drop(x) }` at the binding's enclosing block exit.
//! Escaping the binding (returning it, storing it in non-owned context) defeats
//! the contract — the auto-drop never fires. `Code::OwnershipEscape` flags this
//! at type-check time so the author can either widen the function's return type
//! to `owned<T>` (declaring an ownership transfer) or rework the data flow.

use super::*;
use crate::diagnostic_codes::Code;

fn diagnostic_codes(source: &str) -> Vec<Code> {
    check_source(source).into_iter().map(|d| d.code).collect()
}

#[test]
fn returning_owned_binding_without_owned_return_type_warns() {
    let codes = diagnostic_codes(
        r#"
            pipeline main() {
                fn leak() -> channel {
                    const ch: owned<channel> = channel("leak", 4)
                    return ch
                }
                leak()
            }
        "#,
    );
    assert!(
        codes.contains(&Code::OwnershipEscape),
        "expected HARN-OWN-003 for `return ch` from `owned<channel>` binding, got: {codes:?}"
    );
}

#[test]
fn returning_owned_binding_with_owned_return_type_is_silent() {
    let codes = diagnostic_codes(
        r#"
            pipeline main() {
                fn transfer() -> owned<channel> {
                    const ch: owned<channel> = channel("transfer", 4)
                    return ch
                }
                transfer()
            }
        "#,
    );
    assert!(
        !codes.contains(&Code::OwnershipEscape),
        "ownership transfer through `owned<T>` return type should not warn, got: {codes:?}"
    );
}

#[test]
fn returning_non_owned_binding_is_silent() {
    let codes = diagnostic_codes(
        r#"
            pipeline main() {
                fn passthrough() -> channel {
                    const ch = channel("ok", 4)
                    return ch
                }
                passthrough()
            }
        "#,
    );
    assert!(
        !codes.contains(&Code::OwnershipEscape),
        "non-owned binding return must not fire HARN-OWN-003, got: {codes:?}"
    );
}
