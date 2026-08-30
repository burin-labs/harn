use super::super::prepared_session::{PREPARED_SESSION_COMMANDS, PREPARED_SESSION_STATES};
use super::*;

#[test]
fn generated_bindings_expose_one_prepared_session_contract() {
    let typescript = generate_typescript();
    let rust = generate_rust();
    let swift = generate_swift();
    let python = generate_python();
    let go = generate_go();
    for (artifact, lease, update) in [
        (
            &typescript,
            "HarnPreparedSessionLease",
            "HarnPreparedSessionUpdate",
        ),
        (
            &rust,
            "HarnPreparedSessionLease",
            "HarnPreparedSessionUpdate",
        ),
        (
            &swift,
            "HarnPreparedSessionLease",
            "HarnPreparedSessionUpdate",
        ),
        (
            &python,
            "HarnPreparedSessionLease",
            "HarnPreparedSessionUpdate",
        ),
        (&go, "HarnPreparedSessionLease", "HarnPreparedSessionUpdate"),
    ] {
        assert!(artifact.contains(lease), "binding missing prepared lease");
        assert!(artifact.contains(update), "binding missing prepared update");
        assert!(artifact.contains("harn.prepared_session.v1"));
        for value in PREPARED_SESSION_STATES
            .iter()
            .chain(PREPARED_SESSION_COMMANDS.iter())
        {
            assert!(artifact.contains(value), "binding missing {value}");
        }
    }
    let manifest_json = manifest_json().expect("generate protocol manifest");
    let manifest: serde_json::Value =
        serde_json::from_str(&manifest_json).expect("protocol manifest");
    assert_eq!(
        manifest["preparedSession"]["schema"],
        harn_vm::prepared_run::PREPARED_SESSION_SCHEMA
    );
    assert_eq!(
        manifest["preparedSession"]["states"],
        serde_json::json!(PREPARED_SESSION_STATES)
    );
    assert_eq!(
        manifest["preparedSession"]["commands"],
        serde_json::json!(PREPARED_SESSION_COMMANDS)
    );
}
