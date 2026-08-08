use super::{migrations, render};

/// The generated table must say what the registry says — that is the whole
/// reason for generating it rather than hand-maintaining a second copy.
#[test]
fn every_row_matches_the_runtime_registry() {
    let rows = migrations();
    assert!(
        rows.len() > 100,
        "the registry should carry the whole migrated surface, got {} rows",
        rows.len()
    );
    for (name, replacement) in &rows {
        let migration = harn_vm::stdlib::harness_migration_for_builtin(name)
            .unwrap_or_else(|| panic!("`{name}` is in the table but not in the registry"));
        assert_eq!(
            replacement,
            &format!(
                "harness.{}.{}",
                migration.capability.field_name(),
                migration.method
            ),
            "`{name}` disagrees with the registry"
        );
    }
}

/// `uuid_v7` is the case that motivated harn#6151: the type checker's
/// Levenshtein fallback answered `uuid_v5`, a name-based UUID, for a
/// time-ordered one.
#[test]
fn the_table_carries_the_case_the_fuzzy_matcher_got_wrong() {
    assert_eq!(
        migrations().get("uuid_v7").map(String::as_str),
        Some("harness.random.uuid_v7")
    );
}

/// The registry is keyed by builtin name, so a legacy global can collide with
/// an unrelated typed method. `harn-parser` overrides these four by hand; this
/// asserts the collision is real, so the override cannot quietly become dead
/// code if the registry is corrected upstream.
#[test]
fn the_known_name_collisions_are_still_collisions() {
    let rows = migrations();
    for (legacy, migration, reason) in harn_parser::diagnostic::HARNESS_MIGRATION_DISAGREEMENTS {
        let generated = rows.get(*legacy).map(String::as_str);
        assert!(
            generated.is_some(),
            "`{legacy}` is pinned as a collision but the registry has no row for it"
        );
        assert_ne!(
            generated,
            Some(*migration),
            "`{legacy}` no longer collides — the registry now agrees with the migration, \
             so drop it from HARNESS_MIGRATION_DISAGREEMENTS ({reason})"
        );
    }
}

/// The parser binary-searches the table, so the generator must emit it sorted,
/// and `--check` compares rendered text byte-for-byte.
#[test]
fn the_rendered_table_is_sorted_and_stable() {
    let rendered = render(&migrations());
    let names: Vec<&str> = rendered
        .lines()
        .filter_map(|line| line.trim().strip_prefix('('))
        .filter_map(|line| line.split('"').nth(1))
        .collect();
    assert!(names.len() > 100);
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted, "the generator must emit a sorted table");
    assert_eq!(
        rendered,
        render(&migrations()),
        "two renders of the same registry must be byte-identical"
    );
}
