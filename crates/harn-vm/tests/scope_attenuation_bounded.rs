//! Exhaustive finite-domain check of the production actor-chain validator.
//!
//! Four scope symbols, zero through three delegated actors, every policy mode
//! and both alert settings. This is bounded evidence, not an unbounded proof or
//! an authentication claim. The oracle uses bit membership, not BTreeSet APIs.

use harn_vm::actor_chain::{ActorChain, ScopeAttenuationMode, ScopeAttenuationPolicy};

const SYMBOLS: [&str; 4] = ["fs:read", "fs:write", "net:read", "net:write"];

fn scopes(mask: u8) -> Vec<&'static str> {
    SYMBOLS
        .iter()
        .enumerate()
        .filter_map(|(bit, scope)| (mask & (1 << bit) != 0).then_some(*scope))
        .collect()
}

fn subject(index: usize) -> String {
    format!("principal:{index}")
}

#[test]
fn bounded_scope_attenuation_matches_membership_oracle() {
    let modes = [
        ScopeAttenuationMode::Off,
        ScopeAttenuationMode::NonIncreasing,
        ScopeAttenuationMode::StrictSubset,
    ];
    let mut checked = 0;
    let mut accepted = [0; 3];
    let mut by_depth = [0; 4];

    for actors in 0..=3 {
        // One independent four-bit set for the origin and each actor.
        for assignment in 0_u32..(1 << (4 * (actors + 1))) {
            let masks: Vec<u8> = (0..=actors)
                .map(|hop| ((assignment >> (4 * hop)) & 15) as u8)
                .collect();
            let mut chain = ActorChain::new_with_scopes(subject(0), scopes(masks[0]));
            for (hop, mask) in masks.iter().enumerate().skip(1) {
                chain = chain.pushed_with_scopes(subject(hop), scopes(*mask));
            }
            let unchanged = chain.clone();

            for (mode_index, mode) in modes.iter().copied().enumerate() {
                let expected_bad = (1..=actors).find(|&hop| {
                    let parent = masks[hop - 1];
                    let child = masks[hop];
                    let widens = child & !parent != 0;
                    match mode {
                        ScopeAttenuationMode::Off => false,
                        ScopeAttenuationMode::NonIncreasing => widens,
                        ScopeAttenuationMode::StrictSubset => widens || child == parent,
                    }
                });

                for alert_on_violation in [false, true] {
                    let policy = ScopeAttenuationPolicy {
                        mode,
                        alert_on_violation,
                    };
                    let actual = chain.validate_scope_attenuation(&policy);
                    assert_eq!(
                        actual.is_err(),
                        expected_bad.is_some(),
                        "masks={masks:?} policy={policy:?} actual={actual:?}"
                    );
                    if let Some(hop) = expected_bad {
                        let violation = actual.unwrap_err();
                        assert_eq!(violation.parent_subject(), subject(hop - 1));
                        assert_eq!(violation.child_subject(), subject(hop));
                        assert_eq!(violation.mode(), mode);
                        for (actual_scopes, expected_mask) in [
                            (violation.parent_scopes(), masks[hop - 1]),
                            (violation.child_scopes(), masks[hop]),
                            (violation.extra_scopes(), masks[hop] & !masks[hop - 1]),
                        ] {
                            let actual_scopes: Vec<&str> =
                                actual_scopes.iter().map(String::as_str).collect();
                            assert_eq!(actual_scopes, scopes(expected_mask));
                        }
                    } else {
                        accepted[mode_index] += 1;
                        if mode != ScopeAttenuationMode::Off {
                            // Independently check current-to-origin attenuation.
                            assert_eq!(masks[actors] & !masks[0], 0);
                        }
                    }
                    assert_eq!(chain, unchanged);
                    checked += 1;
                    by_depth[actors] += 1;
                }
            }
        }
    }

    // These counts make zero matches, missing depths and over-restrictive
    // assumptions fail loudly. In particular, Off MUST allow widening.
    assert_eq!(checked, 419_424);
    assert_eq!(by_depth, [96, 1_536, 24_576, 393_216]);
    assert_eq!(accepted, [139_808, 1_956, 550]);
}
