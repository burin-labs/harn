//! Conformance for the open-vocabulary projection across bindings.
//!
//! The class this guards: a producer-owned vocabulary emitted as a CLOSED enum
//! in one language and an open one in another. A closed `String` raw-value
//! Swift enum is not the conservative reading of the same contract, it is a
//! decode failure, and `HarnAgentTerminalOutcome` holds `kind` and `owner`
//! non-optionally, so one unnamed value costs a pinned host the whole payload.

use super::*;

/// The falsifier for #7889. Every producer-owned vocabulary the Rust
/// binding publishes as an open enum must be open in Swift too.
///
/// A closed `String` raw-value enum is not a conservative choice here, it is a
/// decode failure: `Decodable` on a `RawRepresentable` enum throws
/// `DataCorrupted` for a raw value no case names. Because
/// `HarnAgentTerminalOutcome` holds `kind` and `owner` non-optionally, one
/// value a newer Harn adds cost a pinned host the entire terminal outcome
/// rather than one field.
///
/// The assertion is on the shape rather than on a name list alone, so a
/// seventh open vocabulary added to the Rust generator fails here until Swift
/// follows.
#[test]
fn every_open_rust_vocabulary_is_open_in_swift_too() {
    let swift = generate_swift();
    let rust = generate_rust();

    let open_in_rust = [
        "HarnAgentTerminalClass",
        "HarnAgentTerminalKind",
        "HarnAgentTerminalOwner",
        "HarnLlmErrorCategory",
        "HarnLlmErrorKind",
        "HarnLlmErrorReason",
    ];

    for name in open_in_rust {
        // The negative control on the list itself: each name really is open in
        // the Rust artifact, so a stale list cannot make this test vacuous.
        assert!(
            rust.contains(&format!("pub enum {name} {{"))
                && rust.contains("    Unrecognized(String),\n"),
            "{name} is not an open enum in the Rust artifact"
        );

        assert!(
            !swift.contains(&format!(
                "public enum {name}: String, Codable, Sendable, CaseIterable {{"
            )),
            "{name} is still a closed Swift enum, so a value a newer Harn adds fails to decode"
        );
        assert!(
            swift.contains(&format!("public struct {name}: RawRepresentable,")),
            "{name} is missing the open Swift form"
        );
    }

    // The escape itself: the initializer cannot fail, and the vocabulary list
    // is built without the force-unwrap the closed form needs.
    assert!(swift
        .contains("    public init(rawValue: String) {\n        self.rawValue = rawValue\n    }"));
    assert!(swift.contains("    ].map { Self(rawValue: $0) }\n"));

    // Every wire value still reaches the Swift artifact as a named member.
    for value in agent_terminal_kind_values() {
        assert!(
            swift.contains(&format!("= Self(rawValue: {value:?})")),
            "Swift artifact missing terminal kind member for {value}"
        );
    }
}
