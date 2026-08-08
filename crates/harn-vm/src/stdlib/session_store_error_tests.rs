use std::fs;

use harn_session_store::{StoreContention, StoreError};

use super::{open_store, store_error, store_path, ErrorCategory, SessionStoreDir};

#[test]
fn store_contention_maps_to_typed_transient_category() {
    let error = store_error(StoreError::Contention {
        kind: StoreContention::DatabaseBusy,
        message: "database is locked".to_string(),
    });

    assert_eq!(
        crate::value::error_to_category(&error),
        ErrorCategory::ResourceBusy
    );
    assert!(ErrorCategory::ResourceBusy.is_transient());
    assert_eq!(
        error.to_string(),
        "Error [resource_busy]: session_store: retryable backend contention (database_busy): database is locked"
    );
}

#[test]
fn incompatible_store_schema_maps_to_typed_non_retryable_category() {
    let error = store_error(StoreError::SchemaIncompatible {
        schema: "session_store".to_string(),
        stored: 5,
        supported: 2,
    });

    assert_eq!(
        crate::value::error_to_category(&error),
        ErrorCategory::SchemaIncompatible
    );
    assert!(!ErrorCategory::SchemaIncompatible.is_transient());
    assert_eq!(
        error.to_string(),
        "Error [schema_incompatible]: session_store: schema incompatible: session_store version 5 is newer than supported version 2"
    );
}

#[test]
fn canonical_store_open_preserves_newer_schema_category() {
    let root = tempfile::tempdir().expect("tempdir");
    let state_dir = SessionStoreDir::under_root(root.path());
    fs::create_dir_all(state_dir.as_path()).expect("state dir");
    drop(open_store(&state_dir).expect("initialize canonical store"));
    let connection = rusqlite::Connection::open(store_path(&state_dir)).expect("open raw sqlite");
    connection
        .execute(
            "UPDATE _harn_sqlite_schema_versions SET version = 99 WHERE name = 'session_store'",
            [],
        )
        .expect("mark future schema");
    drop(connection);

    let error = match open_store(&state_dir) {
        Ok(_) => panic!("newer canonical store must be rejected"),
        Err(error) => error,
    };
    assert_eq!(
        crate::value::error_to_category(&error),
        ErrorCategory::SchemaIncompatible
    );
    assert!(error
        .to_string()
        .contains("session_store version 99 is newer than supported version 5"));
}
