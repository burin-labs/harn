//! How one repair relates to the whole-program capability pass.
//!
//! These predicates decide whether a repair belongs to a capability
//! migration, must defer to the program-wide plan, or is superseded by it.
//! They are pure classification over a `Repair` id and are kept apart from the
//! planning code so the rules stay readable as one list.

use harn_parser::Repair;

pub(super) fn is_capability_migration_repair(repair: &Repair) -> bool {
    is_capability_migration_repair_id(repair.id.as_str())
}

pub(super) fn is_capability_migration_repair_id(id: &str) -> bool {
    id.starts_with("bindings/thread-harness")
        || matches!(
            id,
            "bindings/thread-missing-harness"
                | "bindings/thread-root-argument"
                | "bindings/prepend-capability-argument"
                | "bindings/attenuate-harness"
                | "bindings/attenuate-capability-argument"
                | "bindings/attenuate-capability-bundle-argument"
                // Attenuation reuses the parameter's existing name, so the
                // migration's own output can leave a narrowed parameter still
                // called `harness`. That is work this migration created, and
                // the pass runs to a fixed point — if the rename were outside
                // the set, every capability migration would end one repair
                // short of clean and never converge.
                | "bindings/name-capability-parameter"
                | "imports/remove-retired-testing-helper"
        )
}

/// Whether a per-file binding repair must yield to the whole-program
/// capability pass for a file that pass is already rewriting.
///
/// Both kinds of repair address a binding, and the whole-program pass is the
/// authority on any binding whose *type* it is about to change. Letting them
/// plan against the same declaration in one pass has no good outcome: the
/// conflict detector marks both unclean, both are skipped, and the migration
/// stalls one repair short of clean forever. Deferring re-plans the local
/// repair on the next pass, against source whose types have settled — which is
/// also when its own analysis is finally correct.
pub(super) fn defers_to_whole_program_pass(id: &str) -> bool {
    matches!(
        id,
        // A binding that looked unused before the program plan may be the
        // carrier used by its emitted call rewrites.
        "bindings/rename-unused"
            // The capability a parameter carries decides what it should be
            // called, so naming it before the pass settles its type names it
            // after a capability it is about to stop carrying.
            | "bindings/name-capability-parameter"
    )
}

pub(super) fn is_whole_program_superseded_repair(repair: &Repair) -> bool {
    let id = repair.id.as_str();
    id.starts_with("bindings/thread-harness")
        || matches!(
            id,
            "bindings/thread-missing-harness"
                | "bindings/thread-root-argument"
                | "bindings/prepend-capability-argument"
        )
}
