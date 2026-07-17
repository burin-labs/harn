use std::sync::Arc;

use super::*;
use crate::value::{DictMap, StructInstanceData, StructLayout, VmEnumVariant, VmSet};

fn s(text: &str) -> VmValue {
    VmValue::String(arcstr::ArcStr::from(text))
}

fn list(items: Vec<VmValue>) -> VmValue {
    VmValue::List(Arc::new(items))
}

fn dict(entries: &[(&str, VmValue)]) -> VmValue {
    let mut map = DictMap::new();
    for (k, v) in entries {
        map.insert(arcstr::ArcStr::from(*k), v.clone());
    }
    VmValue::Dict(Arc::new(map))
}

fn set(items: Vec<VmValue>) -> VmValue {
    let mut out = VmSet::new();
    for item in items {
        out.insert(item);
    }
    VmValue::Set(Arc::new(out))
}

fn render(actual: &VmValue, expected: &VmValue) -> String {
    render_diff(Some("assert_eq failed"), actual, expected).expect("values differ")
}

// -------------------------------------------------------------------------------------------------
// The diff walk
// -------------------------------------------------------------------------------------------------

#[test]
fn equal_values_produce_no_differences() {
    assert!(diff_values(&VmValue::Int(1), &VmValue::Int(1)).is_empty());
    assert!(diff_values(
        &dict(&[("a", VmValue::Int(1))]),
        &dict(&[("a", VmValue::Int(1))])
    )
    .is_empty());
    assert!(render_diff(Some("x"), &VmValue::Int(1), &VmValue::Int(1)).is_none());
}

/// The contract the whole surface rests on: an empty diff and `values_equal`
/// must never disagree, or `assert_eq` could fail with nothing to show.
#[test]
fn diff_is_empty_iff_values_equal() {
    let corpus = vec![
        VmValue::Nil,
        VmValue::Int(0),
        VmValue::Int(1),
        VmValue::Float(1.0),
        VmValue::Bool(true),
        s(""),
        s("1"),
        list(vec![]),
        list(vec![VmValue::Int(1)]),
        list(vec![VmValue::Int(1), VmValue::Int(2)]),
        dict(&[]),
        dict(&[("a", VmValue::Int(1))]),
        dict(&[("a", VmValue::Int(2))]),
        dict(&[("a", VmValue::Int(1)), ("b", VmValue::Nil)]),
        set(vec![VmValue::Int(1)]),
        set(vec![VmValue::Int(1), VmValue::Int(2)]),
    ];
    for a in &corpus {
        for e in &corpus {
            assert_eq!(
                diff_values(a, e).is_empty(),
                values_equal(a, e),
                "diff/equality disagree for {} vs {}",
                repr(a),
                repr(e)
            );
        }
    }
}

#[test]
fn nested_dict_mismatch_is_addressed_by_path() {
    let actual = dict(&[(
        "user",
        dict(&[("name", s("Ada")), ("age", VmValue::Int(36))]),
    )]);
    let expected = dict(&[(
        "user",
        dict(&[("name", s("Grace")), ("age", VmValue::Int(36))]),
    )]);
    let diffs = diff_values(&actual, &expected);
    assert_eq!(diffs.len(), 1);
    assert_eq!(diffs[0].path, ".user.name");
}

#[test]
fn list_index_appears_in_the_path() {
    let actual = list(vec![
        dict(&[("id", VmValue::Int(1))]),
        dict(&[("id", VmValue::Int(9))]),
    ]);
    let expected = list(vec![
        dict(&[("id", VmValue::Int(1))]),
        dict(&[("id", VmValue::Int(2))]),
    ]);
    let diffs = diff_values(&actual, &expected);
    assert_eq!(diffs.len(), 1);
    assert_eq!(diffs[0].path, "[1].id");
}

#[test]
fn extra_and_missing_entries_are_distinguished() {
    let actual = dict(&[("a", VmValue::Int(1)), ("extra", VmValue::Int(3))]);
    let expected = dict(&[("a", VmValue::Int(1)), ("wanted", VmValue::Int(2))]);
    let diffs = diff_values(&actual, &expected);
    assert_eq!(diffs.len(), 2);
    assert_eq!(diffs[0].path, ".extra");
    assert!(matches!(diffs[0].kind, DifferenceKind::Unexpected { .. }));
    assert_eq!(diffs[1].path, ".wanted");
    assert!(matches!(diffs[1].kind, DifferenceKind::Missing { .. }));
}

#[test]
fn differing_lengths_report_only_the_surplus() {
    let diffs = diff_values(
        &list(vec![VmValue::Int(1), VmValue::Int(2), VmValue::Int(3)]),
        &list(vec![VmValue::Int(1), VmValue::Int(2)]),
    );
    assert_eq!(diffs.len(), 1);
    assert_eq!(diffs[0].path, "[2]");
    assert!(matches!(diffs[0].kind, DifferenceKind::Unexpected { .. }));
}

#[test]
fn unrelated_shapes_are_one_whole_mismatch_not_a_field_walk() {
    // A dict compared against a list has no field correspondence to report;
    // inventing one would be worse than saying "these are different things".
    let diffs = diff_values(
        &dict(&[("a", VmValue::Int(1))]),
        &list(vec![VmValue::Int(1)]),
    );
    assert_eq!(diffs.len(), 1);
    assert_eq!(diffs[0].path, "");
    assert!(matches!(diffs[0].kind, DifferenceKind::Unequal { .. }));
}

#[test]
fn nil_and_an_absent_key_are_different_findings() {
    // `{a: nil}` vs `{}` is a missing key, not a value mismatch — the
    // distinction that trips up JSON round-trips.
    let diffs = diff_values(&dict(&[("a", VmValue::Nil)]), &dict(&[]));
    assert_eq!(diffs.len(), 1);
    assert!(matches!(diffs[0].kind, DifferenceKind::Unexpected { .. }));

    // Whereas `{a: nil}` vs `{a: 1}` is a value mismatch at `.a`.
    let diffs = diff_values(
        &dict(&[("a", VmValue::Nil)]),
        &dict(&[("a", VmValue::Int(1))]),
    );
    assert!(matches!(diffs[0].kind, DifferenceKind::Unequal { .. }));
}

#[test]
fn set_diff_reports_membership_not_order() {
    let diffs = diff_values(
        &set(vec![VmValue::Int(1), VmValue::Int(2)]),
        &set(vec![VmValue::Int(2), VmValue::Int(3)]),
    );
    assert_eq!(diffs.len(), 2);
    assert_eq!(diffs[0].path, "{1}");
    assert!(matches!(diffs[0].kind, DifferenceKind::Unexpected { .. }));
    assert_eq!(diffs[1].path, "{3}");
    assert!(matches!(diffs[1].kind, DifferenceKind::Missing { .. }));
}

fn struct_value(name: &str, fields: &[(&str, VmValue)]) -> VmValue {
    let layout = StructLayout::new(
        name.to_string(),
        fields.iter().map(|(k, _)| (*k).to_string()).collect(),
    );
    VmValue::StructInstance(Arc::new(StructInstanceData {
        layout: Arc::new(layout),
        fields: Arc::new(fields.iter().map(|(_, v)| Some(v.clone())).collect()),
    }))
}

fn variant(enum_name: &str, variant: &str, fields: Vec<VmValue>) -> VmValue {
    VmValue::EnumVariant(Arc::new(VmEnumVariant {
        enum_name: arcstr::ArcStr::from(enum_name),
        variant: arcstr::ArcStr::from(variant),
        fields: Arc::new(fields),
    }))
}

#[test]
fn struct_fields_are_addressed_like_field_access() {
    let diffs = diff_values(
        &struct_value("User", &[("name", s("Ada")), ("age", VmValue::Int(36))]),
        &struct_value("User", &[("name", s("Ada")), ("age", VmValue::Int(37))]),
    );
    assert_eq!(diffs.len(), 1);
    assert_eq!(diffs[0].path, ".age");
}

/// Two different structs are not a field-by-field disagreement, however much
/// their fields happen to line up — they are different things.
#[test]
fn differently_named_structs_are_one_whole_mismatch() {
    let diffs = diff_values(
        &struct_value("User", &[("id", VmValue::Int(1))]),
        &struct_value("Account", &[("id", VmValue::Int(1))]),
    );
    assert_eq!(diffs.len(), 1);
    assert_eq!(diffs[0].path, "");
}

/// An enum payload is reached as `value.fields[i]`, so the path says that. A
/// bare `[0]` would read as a list index into something that is not a list.
#[test]
fn enum_payload_is_addressed_the_way_harn_reaches_it() {
    let diffs = diff_values(
        &variant("Status", "Active", vec![VmValue::Int(1)]),
        &variant("Status", "Active", vec![VmValue::Int(2)]),
    );
    assert_eq!(diffs.len(), 1);
    assert_eq!(diffs[0].path, ".fields[0]");
}

#[test]
fn a_different_variant_is_one_whole_mismatch_and_renders_readably() {
    assert_eq!(
        render(
            &variant("Status", "Active", vec![VmValue::Int(1)]),
            &variant("Status", "Idle", vec![]),
        ),
        "\
assert_eq failed.
    expected  Status::Idle
    actual    Status::Active(1)"
    );
}

#[test]
fn non_identifier_keys_are_addressed_with_bracket_syntax() {
    let diffs = diff_values(
        &dict(&[("content-type", s("text/plain"))]),
        &dict(&[("content-type", s("application/json"))]),
    );
    assert_eq!(diffs[0].path, "[\"content-type\"]");
}

// -------------------------------------------------------------------------------------------------
// Rendering — the failure text is the feature, so it is asserted verbatim.
// -------------------------------------------------------------------------------------------------

#[test]
fn scalar_mismatch_renders_without_ceremony() {
    assert_eq!(
        render(&VmValue::Int(3), &VmValue::Int(4)),
        "\
assert_eq failed.
    expected  4
    actual    3"
    );
}

#[test]
fn a_type_mismatch_names_both_types() {
    // Without the type tags these two lines would read `"1"` and `1` — and
    // with a plain `display()` they would read identically.
    assert_eq!(
        render(&VmValue::Int(1), &s("1")),
        "\
assert_eq failed.
    expected  \"1\" (string)
    actual    1 (int)
    One side is a number and the other is text. If this came from parsed input, the conversion may be missing."
    );
}

#[test]
fn nested_mismatch_renders_path_and_only_the_differing_leaf() {
    let actual = dict(&[
        ("id", VmValue::Int(7)),
        ("user", dict(&[("name", s("Ada")), ("team", s("core"))])),
    ]);
    let expected = dict(&[
        ("id", VmValue::Int(7)),
        ("user", dict(&[("name", s("Grace")), ("team", s("core"))])),
    ]);
    assert_eq!(
        render(&actual, &expected),
        "\
assert_eq failed: the two values differ in 1 place.

  at .user.name
    expected  \"Grace\"
    actual    \"Ada\"
    The strings first differ at character 0."
    );
}

#[test]
fn list_of_dicts_renders_every_differing_leaf_with_its_index() {
    let actual = list(vec![
        dict(&[("n", VmValue::Int(1))]),
        dict(&[("n", VmValue::Int(9))]),
    ]);
    let expected = list(vec![
        dict(&[("n", VmValue::Int(1))]),
        dict(&[("n", VmValue::Int(2))]),
        dict(&[("n", VmValue::Int(3))]),
    ]);
    assert_eq!(
        render(&actual, &expected),
        "\
assert_eq failed: the two values differ in 2 places.

  at [1].n
    expected  2
    actual    9

  at [2]
    expected  {\"n\": 3}
    actual    nothing here"
    );
}

#[test]
fn float_mismatch_points_at_the_tolerance_escape_hatch() {
    let rendered = render(&VmValue::Float(0.30000000000000004), &VmValue::Float(0.3));
    assert!(
        rendered.contains("Floating-point arithmetic is inexact"),
        "{rendered}"
    );
    assert!(rendered.contains("assert_approx"), "{rendered}");
    assert!(
        rendered.contains("These differ by 5.551115123125783e-17"),
        "{rendered}"
    );
}

#[test]
fn long_values_are_abbreviated_in_the_middle_keeping_both_ends() {
    // A long string usually differs at one end, so a trailing ellipsis alone
    // would hide exactly what the reader came to see. Both ends survive.
    let long = s(&format!("HEAD{}TAIL", "x".repeat(400)));
    let rendered = render(&long, &s("short"));
    assert!(
        rendered.contains("HEAD"),
        "the head must survive: {rendered}"
    );
    assert!(
        rendered.contains("TAIL"),
        "the tail must survive: {rendered}"
    );
    assert!(rendered.contains('…'), "{rendered}");
    // The reader is told how much was withheld, and how much there was.
    assert!(
        rendered.contains("(410 characters in all)"),
        "the full length must be stated: {rendered}"
    );
    assert!(
        !rendered.contains(&"x".repeat(200)),
        "the middle must actually be dropped: {rendered}"
    );
    for line in rendered.lines() {
        assert!(line.chars().count() < 200, "line stayed readable: {line}");
    }
}

#[test]
fn a_flood_of_differences_is_capped_and_counted() {
    let actual = list((0..30).map(VmValue::Int).collect());
    let expected = list((100..130).map(VmValue::Int).collect());
    let rendered = render(&actual, &expected);
    assert!(rendered.contains("differ in 30 places"), "{rendered}");
    assert!(
        rendered.ends_with("… and 20 more differences."),
        "{rendered}"
    );
    assert_eq!(rendered.matches("  at [").count(), MAX_DIFFERENCES);
}

#[test]
fn one_suppressed_difference_is_singular() {
    let actual = list((0..11).map(VmValue::Int).collect());
    let expected = list((100..111).map(VmValue::Int).collect());
    assert!(render(&actual, &expected).ends_with("… and 1 more difference."));
}

#[test]
fn repr_distinguishes_values_that_display_identically() {
    assert_eq!(repr(&s("1")), "\"1\"");
    assert_eq!(repr(&VmValue::Int(1)), "1");
    assert_eq!(repr(&s("nil")), "\"nil\"");
    assert_eq!(repr(&VmValue::Nil), "nil");
    assert_eq!(repr(&s("a\nb")), "\"a\\nb\"");
    assert_eq!(repr(&s("say \"hi\"")), "\"say \\\"hi\\\"\"");
    assert_eq!(repr(&dict(&[("a", s("b"))])), "{\"a\": \"b\"}");
    assert_eq!(repr(&list(vec![s("a"), VmValue::Int(1)])), "[\"a\", 1]");
}

/// Set rendering sorts, so identical sets built in different orders produce
/// identical output — failure text must not depend on insertion order.
#[test]
fn set_repr_is_order_independent() {
    let a = set(vec![VmValue::Int(3), VmValue::Int(1), VmValue::Int(2)]);
    let b = set(vec![VmValue::Int(1), VmValue::Int(2), VmValue::Int(3)]);
    assert_eq!(repr(&a), repr(&b));
}

/// Diff output is asserted on byte-for-byte by conformance tests, so it must
/// not vary between runs.
#[test]
fn rendering_is_deterministic() {
    let actual = dict(&[
        ("z", VmValue::Int(1)),
        ("a", VmValue::Int(2)),
        ("m", VmValue::Int(3)),
    ]);
    let expected = dict(&[
        ("z", VmValue::Int(9)),
        ("a", VmValue::Int(9)),
        ("m", VmValue::Int(9)),
    ]);
    let first = render(&actual, &expected);
    for _ in 0..20 {
        assert_eq!(render(&actual, &expected), first);
    }
    // …and in sorted-key order, which is the order a reader scans in.
    let paths: Vec<&str> = first
        .lines()
        .filter_map(|l| l.strip_prefix("  at "))
        .collect();
    assert_eq!(paths, vec![".a", ".m", ".z"]);
}
