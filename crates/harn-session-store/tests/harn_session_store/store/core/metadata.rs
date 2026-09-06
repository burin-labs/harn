//! Titles, pinning, attribute merges, and the change observer both backends publish to.

use super::super::*;

/// The pin exists so a person's title outranks a generated one. Walking the
/// whole intent table in one session proves the rule holds in both directions
/// and across repeated derived writes, not just on the first collision.
#[tokio::test]
async fn derived_title_writes_yield_to_a_pinned_title_in_both_backends() {
    run_with_hooks(StoreHooks::default(), |store| async move {
        let session = store
            .create(CreateSession {
                id: Some("titled".into()),
                ..CreateSession::default()
            })
            .await
            .expect("create session");
        assert_eq!(
            (session.title.as_deref(), session.title_pinned),
            (None, false)
        );

        // A derived title lands while nothing is pinned.
        let derived = store
            .update(
                &session.id,
                UpdateSession {
                    title: Some("Investigate flaky retries".into()),
                    ..UpdateSession::default()
                },
            )
            .await
            .expect("derived title");
        assert_eq!(
            (derived.title.as_deref(), derived.title_pinned),
            (Some("Investigate flaky retries"), false)
        );

        // A rename claims the title and pins it.
        let renamed = store
            .update(
                &session.id,
                UpdateSession {
                    title: Some("Retry storm".into()),
                    title_pinned: Some(true),
                    ..UpdateSession::default()
                },
            )
            .await
            .expect("rename");
        assert_eq!(
            (renamed.title.as_deref(), renamed.title_pinned),
            (Some("Retry storm"), true)
        );

        // Two further derived writes are accepted calls that change nothing.
        for generated in ["Fix the retry backoff", "Adjust exponential backoff"] {
            let ignored = store
                .update(
                    &session.id,
                    UpdateSession {
                        title: Some(generated.into()),
                        model: Some("model-v3".into()),
                        ..UpdateSession::default()
                    },
                )
                .await
                .expect("derived write must not error against a pinned title");
            assert_eq!(
                (ignored.title.as_deref(), ignored.title_pinned),
                (Some("Retry storm"), true)
            );
            // Only the title is protected; the rest of the update still lands.
            assert_eq!(ignored.model.as_deref(), Some("model-v3"));
        }

        // Renaming again wins regardless of the prior pin.
        let renamed_again = store
            .update(
                &session.id,
                UpdateSession {
                    title: Some("Retry storm, part two".into()),
                    title_pinned: Some(true),
                    ..UpdateSession::default()
                },
            )
            .await
            .expect("second rename");
        assert_eq!(
            (renamed_again.title.as_deref(), renamed_again.title_pinned),
            (Some("Retry storm, part two"), true)
        );

        // Releasing the pin alone keeps the title the user last chose.
        let released = store
            .update(
                &session.id,
                UpdateSession {
                    title_pinned: Some(false),
                    ..UpdateSession::default()
                },
            )
            .await
            .expect("release pin");
        assert_eq!(
            (released.title.as_deref(), released.title_pinned),
            (Some("Retry storm, part two"), false)
        );

        // With the pin released, auto-titling owns the title again.
        let resumed = store
            .update(
                &session.id,
                UpdateSession {
                    title: Some("Tune backoff ceiling".into()),
                    ..UpdateSession::default()
                },
            )
            .await
            .expect("derived title after release");
        assert_eq!(
            (resumed.title.as_deref(), resumed.title_pinned),
            (Some("Tune backoff ceiling"), false)
        );

        // A session created already pinned resists derived writes from birth,
        // which is what a rename against a not-yet-persisted session needs.
        let seeded = store
            .create(CreateSession {
                id: Some("seeded".into()),
                title: Some("Chosen at creation".into()),
                title_pinned: true,
                ..CreateSession::default()
            })
            .await
            .expect("create pinned session");
        assert!(seeded.title_pinned);
        let after = store
            .update(
                &seeded.id,
                UpdateSession {
                    title: Some("Generated instead".into()),
                    ..UpdateSession::default()
                },
            )
            .await
            .expect("derived write against seeded pin");
        assert_eq!(after.title.as_deref(), Some("Chosen at creation"));
    })
    .await;
}

/// A pin that lived only in memory would be worthless: the whole point is that
/// the title survives to the next launch.
#[tokio::test]
async fn sqlite_pin_survives_reopen() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("pinned.sqlite");
    let store = SqliteSessionStore::open(&path).expect("open sqlite");
    store
        .create(CreateSession {
            id: Some("persisted".into()),
            ..CreateSession::default()
        })
        .await
        .expect("create");
    store
        .update(
            "persisted",
            UpdateSession {
                title: Some("Name I chose".into()),
                title_pinned: Some(true),
                ..UpdateSession::default()
            },
        )
        .await
        .expect("rename");
    drop(store);

    let store = SqliteSessionStore::open(&path).expect("reopen sqlite");
    let reopened = store.describe("persisted").await.expect("describe");
    assert_eq!(
        (reopened.title.as_deref(), reopened.title_pinned),
        (Some("Name I chose"), true)
    );
    let derived = store
        .update(
            "persisted",
            UpdateSession {
                title: Some("Generated after restart".into()),
                ..UpdateSession::default()
            },
        )
        .await
        .expect("derived write after reopen");
    assert_eq!(derived.title.as_deref(), Some("Name I chose"));
}

/// Databases written before pinning existed record no user choice, so they must
/// migrate to unpinned rather than fail to open or claim a pin nobody set.
#[tokio::test]
async fn sqlite_database_without_the_pin_column_migrates_to_unpinned() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("legacy.sqlite");
    let store = SqliteSessionStore::open(&path).expect("open sqlite");
    store
        .create(CreateSession {
            id: Some("legacy".into()),
            title: Some("Title from before pinning".into()),
            ..CreateSession::default()
        })
        .await
        .expect("create");
    drop(store);

    // Stand in for a real pre-pin database: drop the column *and* roll the
    // recorded version back to v4. Version alone is what the shared
    // initializer consults, so a test that dropped only the column would
    // describe a v5 file and never exercise the upgrade.
    let conn = rusqlite::Connection::open(&path).expect("open legacy connection");
    conn.execute_batch(
        "ALTER TABLE sessions DROP COLUMN title_pinned;
         UPDATE _harn_sqlite_schema_versions SET version = 4 WHERE name = 'session_store';",
    )
    .expect("simulate pre-pin schema");
    drop(conn);

    let store = SqliteSessionStore::open(&path).expect("reopen and migrate");
    let migrated = store.describe("legacy").await.expect("describe");
    assert_eq!(
        (migrated.title.as_deref(), migrated.title_pinned),
        (Some("Title from before pinning"), false)
    );

    // The migrated row is a normal row: it still accepts a rename and then
    // defends it.
    store
        .update(
            "legacy",
            UpdateSession {
                title: Some("Renamed after upgrade".into()),
                title_pinned: Some(true),
                ..UpdateSession::default()
            },
        )
        .await
        .expect("rename migrated row");
    let defended = store
        .update(
            "legacy",
            UpdateSession {
                title: Some("Generated".into()),
                ..UpdateSession::default()
            },
        )
        .await
        .expect("derived write");
    assert_eq!(
        (defended.title.as_deref(), defended.title_pinned),
        (Some("Renamed after upgrade"), true)
    );
}

/// Records every committed metadata change it is told about.
#[derive(Default)]
struct RecordingObserver {
    seen: std::sync::Mutex<Vec<(String, Option<String>, bool)>>,
}

impl harn_session_store::SessionChangeObserver for RecordingObserver {
    fn session_updated(&self, meta: &SessionMeta) {
        self.seen.lock().expect("observer lock").push((
            meta.id.clone(),
            meta.title.clone(),
            meta.title_pinned,
        ));
    }
}

/// A surface that already showed a session's name only learns the name moved
/// if `update` publishes the committed row. Both backends must publish, or the
/// notification silently depends on which one a deployment happens to run.
///
/// This also pins *what* is published: the row as resolved by
/// `resolve_title_update`, not the caller's request. A derived write that lost
/// to a pinned title must report the title that actually stands, otherwise a
/// live surface renders a name the store does not hold.
#[tokio::test]
async fn committed_metadata_change_is_published_by_both_backends() {
    let observer = Arc::new(RecordingObserver::default());
    let hooks = StoreHooks {
        change_observer: Some(observer.clone()),
        ..StoreHooks::default()
    };
    run_with_hooks(hooks, |store| {
        let observer = observer.clone();
        async move {
            let before = observer.seen.lock().expect("observer lock").len();
            store
                .create(CreateSession {
                    id: Some("observed".into()),
                    ..CreateSession::default()
                })
                .await
                .expect("create session");
            assert_eq!(
                observer.seen.lock().expect("observer lock").len(),
                before,
                "create is not a metadata change and must not publish"
            );

            store
                .update(
                    "observed",
                    UpdateSession {
                        title: Some("derived name".into()),
                        ..UpdateSession::default()
                    },
                )
                .await
                .expect("derived update");
            store
                .update(
                    "observed",
                    UpdateSession {
                        title: Some("a name a person chose".into()),
                        title_pinned: Some(true),
                        ..UpdateSession::default()
                    },
                )
                .await
                .expect("pinning rename");
            store
                .update(
                    "observed",
                    UpdateSession {
                        title: Some("a later derived name".into()),
                        ..UpdateSession::default()
                    },
                )
                .await
                .expect("derived update after pin");

            let seen = observer.seen.lock().expect("observer lock");
            assert_eq!(
                seen[before..],
                [
                    (
                        "observed".to_string(),
                        Some("derived name".to_string()),
                        false
                    ),
                    (
                        "observed".to_string(),
                        Some("a name a person chose".to_string()),
                        true
                    ),
                    (
                        "observed".to_string(),
                        Some("a name a person chose".to_string()),
                        true
                    ),
                ],
                "each committed update publishes the title that stands, not the one requested"
            );
        }
    })
    .await;
}

/// An attribute learned after the session exists can be recorded, and recording
/// it does not erase the ones already there.
///
/// Attributes used to be reachable only through `create`, which made every one
/// of them a create-time fact. A writer that learned something later — the
/// provider a run actually used, say — had no way to store it and no way to
/// find out it had not: the value simply never appeared, and the read came back
/// null as if nothing had ever been known.
///
/// The empty-map case at the end is the falsifier. A replace-shaped
/// implementation passes every other assertion here and fails only that one, by
/// clearing the map when a caller who knows nothing about attributes performs an
/// ordinary title update.
#[tokio::test]
async fn an_attribute_learned_after_creation_merges_without_erasing_the_others() {
    run_with_hooks(StoreHooks::default(), |store| async move {
        let session = store
            .create(CreateSession {
                id: Some("late-attribute".into()),
                attributes: std::collections::BTreeMap::from([
                    ("source".to_string(), json!("importer")),
                    ("build".to_string(), json!("1.2.3")),
                ]),
                ..CreateSession::default()
            })
            .await
            .expect("create session");

        let updated = store
            .update(
                &session.id,
                UpdateSession {
                    attributes: std::collections::BTreeMap::from([
                        // Learned late: absent at create.
                        ("provider".to_string(), json!("fixture")),
                        // Learned again: a later writer corrects an earlier one.
                        ("build".to_string(), json!("1.2.4")),
                    ]),
                    ..UpdateSession::default()
                },
            )
            .await
            .expect("merge attributes");

        assert_eq!(
            updated.attributes.get("provider"),
            Some(&json!("fixture")),
            "an attribute learned after creation must be recorded"
        );
        assert_eq!(
            updated.attributes.get("build"),
            Some(&json!("1.2.4")),
            "a key present in the update must win"
        );
        assert_eq!(
            updated.attributes.get("source"),
            Some(&json!("importer")),
            "a key absent from the update must survive it"
        );

        // THE FALSIFIER. A caller that knows nothing about attributes must not
        // clear them by touching anything else.
        let after_title = store
            .update(
                &session.id,
                UpdateSession {
                    title: Some("unrelated".into()),
                    ..UpdateSession::default()
                },
            )
            .await
            .expect("update title only");
        assert_eq!(
            after_title.attributes, updated.attributes,
            "an empty attribute map is no change, never a request to clear"
        );

        // Durability, not just the returned value: the merge must be readable
        // back through a fresh describe rather than only in the write's echo.
        let described = store.describe(&session.id).await.expect("describe");
        assert_eq!(
            described.attributes, updated.attributes,
            "the merged attributes must be what the store actually holds"
        );
    })
    .await;
}
