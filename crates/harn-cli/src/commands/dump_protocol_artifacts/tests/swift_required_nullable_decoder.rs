//! The generated Swift keeps enforcing a required-nullable key.
//!
//! A required-nullable field is `T?` in Swift whichever way it is declared, so
//! Codable synthesis decodes it with `decodeIfPresent` and reads an omitted key
//! as `nil`. Only a custom `init(from:)` calling `decode(T?.self)` tells the
//! two apart, and telling them apart is the whole point: the schema says the
//! emitter always writes the key, so a frame without it is drift.
//!
//! The emitter used to gate that initializer on an unrelated fact — whether
//! some other field was a non-required JSON value. A struct therefore kept its
//! enforcement only for as long as that neighbour stayed optional, and the
//! transcript-compacted meta struct lost it, alone among eleven, when its
//! optional JSON fields were tightened into required ones. The custom encoder
//! stayed behind, so the artifact still wrote every key while quietly
//! accepting frames that omitted one.

use super::super::swift::generate_swift_for_tests;

/// One `public struct` body, from its opening brace to the closing brace in
/// column zero.
fn struct_bodies(swift: &str) -> Vec<(String, String)> {
    let mut bodies = Vec::new();
    for chunk in swift.split("\npublic struct ").skip(1) {
        let Some(name) = chunk.split([':', ' ', '\n']).next() else {
            continue;
        };
        let body = chunk.split("\n}\n").next().unwrap_or(chunk);
        bodies.push((name.to_string(), body.to_string()));
    }
    bodies
}

/// A struct that needs a custom encoder needs the custom decoder with it.
///
/// Both are emitted for the same reason and from the same fact: a
/// required-nullable field whose presence Codable synthesis cannot express.
/// Asserting the pair rather than either half is what makes this survive the
/// next change to the condition, because the failure being pinned was exactly
/// the two coming apart.
#[test]
fn every_struct_with_a_custom_encoder_carries_the_custom_decoder() {
    let swift = generate_swift_for_tests();
    let bodies = struct_bodies(&swift);
    assert!(
        !bodies.is_empty(),
        "no structs were parsed out of the Swift artifact, so this asserts nothing"
    );

    let mut paired = 0usize;
    for (name, body) in &bodies {
        if !body.contains("public func encode(to encoder: Encoder) throws") {
            continue;
        }
        assert!(
            body.contains("public init(from decoder: Decoder) throws"),
            "{name} emits a custom encoder but no custom decoder, so a key it \
             always writes is read back with `decodeIfPresent` and an omitted \
             key decodes as nil instead of being refused"
        );
        paired += 1;
    }
    assert!(
        paired > 0,
        "no struct emits a custom encoder, so the pairing above was never tested"
    );
}

/// The regression by name, and the control that says the gate is not simply
/// on for everything.
#[test]
fn the_compaction_struct_refuses_an_omitted_required_nullable_key() {
    let swift = generate_swift_for_tests();
    let bodies = struct_bodies(&swift);
    let body = |wanted: &str| {
        bodies
            .iter()
            .find(|(name, _)| name == wanted)
            .map(|(_, body)| body.clone())
            .unwrap_or_else(|| panic!("{wanted} is missing from the Swift artifact"))
    };

    // `snapshotAssetId` is required with type ["string", "null"]. Reading it
    // unconditionally is what refuses a frame that leaves the key out.
    let compacted = body("HarnACPTranscriptCompactedUpdateMetaHarn");
    assert!(
        compacted.contains("snapshotAssetId = try values.decode(String?.self, forKey: .snapshotAssetId)"),
        "the compaction struct must read its required-nullable key unconditionally, got:\n{compacted}"
    );

    // Control: a struct that has a non-required JSON field keeps the
    // present-versus-absent read, so the fix widened the condition rather
    // than replacing one arm of it.
    let log = body("HarnACPLogUpdateMetaHarn");
    assert!(
        log.contains("values.contains(.fields)"),
        "the log struct must still distinguish an absent JSON field from a null one, got:\n{log}"
    );
}
