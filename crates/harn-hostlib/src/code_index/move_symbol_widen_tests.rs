//! `allow_visibility_widening` and the visibility checks around it.

use super::*;

#[test]
fn rust_nested_import_does_not_stand_in_for_the_sources_own_import() {
    // The tests module imports the moved struct itself, but the module body
    // uses it too and needs its own import from the new home.
    let fixture = Fixture::new(&[
        KIT,
        ("src/lib.rs", "pub mod a;\npub mod b;\n"),
        (
            "src/a.rs",
            "pub struct Item {\n    pub id: u8,\n}\n\npub fn make() -> Item {\n    Item { id: 1 }\n}\n\n#[cfg(test)]\nmod tests {\n    use super::Item;\n\n    #[test]\n    fn builds() {\n        let _ = Item { id: 2 };\n    }\n}\n",
        ),
        ("src/b.rs", "pub fn other() {}\n"),
    ]);
    let result = fixture.mv("Item", "src/a.rs", "src/b.rs");
    assert_eq!(tag(&result), "applied", "{result:?}");
    let source = fixture.read("src/a.rs");
    assert!(
        source.starts_with("use crate::b::Item;\n\npub fn make() -> Item {"),
        "{source}"
    );
    assert!(source.contains("    use crate::b::Item;\n"), "{source}");
}

// === Visibility widening ===

const KIT: (&str, &str) = (
    "Cargo.toml",
    "[package]\nname = \"kit\"\nversion = \"0.1.0\"\n",
);

fn widened(result: &VmValue) -> Vec<(String, i64, String, String)> {
    list(field(result, "widened"))
        .iter()
        .map(|w| {
            (
                s(field(w, "path")),
                int(field(w, "line")),
                s(field(w, "from")),
                s(field(w, "to")),
            )
        })
        .collect()
}

fn widening_args<'a>(
    symbol: &'a str,
    path: &'a str,
    to_path: &'a str,
    allow: Option<&'a str>,
) -> Vec<(&'a str, VmValue)> {
    let mut args = vec![
        ("symbol", vm_string(symbol)),
        ("path", vm_string(path)),
        ("to_path", vm_string(to_path)),
    ];
    if let Some(level) = allow {
        args.push(("allow_visibility_widening", vm_string(level)));
    }
    args
}

/// `a` and `b` are siblings under the crate root, so `pub(super)` on an item
/// of `a` already reaches `b`.
fn sibling_modules_fixture() -> Fixture {
    Fixture::new(&[
        KIT,
        ("src/lib.rs", "pub mod a;\npub mod b;\n"),
        (
            "src/a.rs",
            "fn helper() -> u8 {\n    1\n}\n\nfn untouched() -> u8 {\n    2\n}\n\npub fn caller() -> u8 {\n    helper()\n}\n",
        ),
        ("src/b.rs", "pub fn other() {}\n"),
    ])
}

#[test]
fn rust_widening_uses_the_narrowest_level_and_leaves_other_items_alone() {
    let fixture = sibling_modules_fixture();
    // The allowance is looser than needed: only `pub(super)` is applied.
    let result = fixture.move_symbol(&widening_args(
        "caller",
        "src/a.rs",
        "src/b.rs",
        Some("pub"),
    ));
    assert_eq!(tag(&result), "applied", "{result:?}");
    assert_eq!(
        widened(&result),
        [(
            "src/a.rs".to_string(),
            1,
            "private".to_string(),
            "pub(super)".to_string()
        )]
    );
    assert_eq!(
        fixture.read("src/a.rs"),
        "pub(super) fn helper() -> u8 {\n    1\n}\n\nfn untouched() -> u8 {\n    2\n}\n"
    );
    let moved = fixture.read("src/b.rs");
    assert!(moved.contains("use crate::a::helper;"), "{moved}");
    assert!(
        moved.contains("pub fn caller() -> u8 {\n    helper()\n}"),
        "{moved}"
    );
}

#[test]
fn rust_widening_defaults_to_refusing_and_names_the_level() {
    let fixture = sibling_modules_fixture();
    let result = fixture.refused(
        "visibility_required",
        &widening_args("caller", "src/a.rs", "src/b.rs", None),
    );
    assert_eq!(s(field(&result, "required_visibility")), "pub(super)");
    assert!(list(field(&result, "widened")).is_empty());
    let sites = list(field(&result, "sites"));
    assert_eq!(sites.len(), 1, "{sites:?}");
    assert_eq!(int(field(&sites[0], "line")), 1);
}

#[test]
fn rust_widening_beyond_the_allowance_refuses() {
    // `a::model` and `b::moved` share only the crate root, so `pub(super)`
    // on a `a::model` item cannot reach `b::moved`: `pub(crate)` is needed.
    let fixture = Fixture::new(&[
        KIT,
        ("src/lib.rs", "pub mod a;\npub mod b;\n"),
        ("src/a/mod.rs", "pub mod model;\n"),
        (
            "src/a/model.rs",
            "fn helper() -> u8 {\n    1\n}\n\npub fn caller() -> u8 {\n    helper()\n}\n",
        ),
        ("src/b/mod.rs", "pub mod moved;\n"),
        ("src/b/moved.rs", "pub fn other() {}\n"),
    ]);
    let result = fixture.refused(
        "visibility_required",
        &widening_args(
            "caller",
            "src/a/model.rs",
            "src/b/moved.rs",
            Some("pub(super)"),
        ),
    );
    assert_eq!(s(field(&result, "required_visibility")), "pub(crate)");
    assert!(
        s(field(&result, "details")).contains("`pub(crate)`"),
        "{result:?}"
    );
    let applied = fixture.move_symbol(&widening_args(
        "caller",
        "src/a/model.rs",
        "src/b/moved.rs",
        Some("pub(crate)"),
    ));
    assert_eq!(tag(&applied), "applied", "{applied:?}");
    assert_eq!(
        widened(&applied),
        [(
            "src/a/model.rs".to_string(),
            1,
            "private".to_string(),
            "pub(crate)".to_string()
        )]
    );
}

#[test]
fn rust_widening_covers_private_fields_and_the_moved_item_itself() {
    let fields = Fixture::new(&[
        KIT,
        ("src/lib.rs", "pub mod a;\npub mod b;\n"),
        (
            "src/a.rs",
            "pub struct Job {\n    level: u8,\n    note: u8,\n}\n\npub fn level(job: &Job) -> u8 {\n    job.level\n}\n",
        ),
        ("src/b.rs", "pub fn other() {}\n"),
    ]);
    let result = fields.move_symbol(&widening_args(
        "level",
        "src/a.rs",
        "src/b.rs",
        Some("pub(crate)"),
    ));
    assert_eq!(tag(&result), "applied", "{result:?}");
    assert_eq!(
        widened(&result),
        [(
            "src/a.rs".to_string(),
            2,
            "private".to_string(),
            "pub(super)".to_string()
        )]
    );
    assert_eq!(
        fields.read("src/a.rs"),
        "pub struct Job {\n    pub(super) level: u8,\n    note: u8,\n}\n"
    );

    // A private item the source keeps using is widened where it lands.
    let item = Fixture::new(&[
        KIT,
        ("src/lib.rs", "pub mod a;\npub mod b;\n"),
        (
            "src/a.rs",
            "fn helper() -> u8 {\n    1\n}\n\npub fn caller() -> u8 {\n    helper()\n}\n",
        ),
        ("src/b.rs", "pub fn other() {}\n"),
    ]);
    let refused = item.refused(
        "visibility_required",
        &widening_args("helper", "src/a.rs", "src/b.rs", None),
    );
    assert_eq!(s(field(&refused, "required_visibility")), "pub(super)");
    let result = item.move_symbol(&widening_args(
        "helper",
        "src/a.rs",
        "src/b.rs",
        Some("pub(super)"),
    ));
    assert_eq!(tag(&result), "applied", "{result:?}");
    assert_eq!(
        widened(&result),
        [(
            "src/a.rs".to_string(),
            1,
            "private".to_string(),
            "pub(super)".to_string()
        )]
    );
    let moved = item.read("src/b.rs");
    assert!(moved.contains("pub(super) fn helper() -> u8 {"), "{moved}");
    assert!(item.read("src/a.rs").contains("use crate::b::helper;"));
}

#[test]
fn rust_widening_replaces_a_restricted_field_for_the_modules_that_read_it() {
    let fixture = restricted_fields_fixture(&[
        (
            "src/a/mod.rs",
            "pub mod model;\n\npub fn level(m: &model::Meta) -> u8 {\n    m.level\n}\n",
        ),
        ("src/a/model/child.rs", "pub fn noop() {}\n"),
    ]);
    let refused = fixture.refused(
        "visibility_required",
        &widening_args(
            "Meta",
            "src/a/model.rs",
            "src/b/moved.rs",
            Some("pub(super)"),
        ),
    );
    assert_eq!(s(field(&refused, "required_visibility")), "pub(crate)");
    let result = fixture.move_symbol(&widening_args(
        "Meta",
        "src/a/model.rs",
        "src/b/moved.rs",
        Some("pub(crate)"),
    ));
    assert_eq!(tag(&result), "applied", "{result:?}");
    // Only the field `a` reads is widened; the private `id` nobody reads stays.
    assert_eq!(
        widened(&result),
        [(
            "src/a/model.rs".to_string(),
            4,
            "pub(super)".to_string(),
            "pub(crate)".to_string()
        )]
    );
    let moved = fixture.read("src/b/moved.rs");
    assert!(
        moved.contains("pub(crate) level: u8,\n    pub(crate) target: u8,\n    id: u8,\n"),
        "{moved}"
    );
}

#[test]
fn rust_widening_never_covers_constructors_or_restricted_paths() {
    // A struct update copies every field, so widening one read is not enough.
    let fixture = restricted_fields_fixture(&[
        ("src/a/mod.rs", "pub mod model;\n"),
        (
            "src/a/model/child.rs",
            "use super::Meta;\n\npub fn copy(m: &Meta) -> Meta {\n    Meta { target: 2, ..*m }\n}\n",
        ),
    ]);
    fixture.refused(
        "visibility_required",
        &widening_args("Meta", "src/a/model.rs", "src/b/moved.rs", Some("pub")),
    );
}

#[test]
fn widening_levels_must_belong_to_the_language() {
    let rust = sibling_modules_fixture();
    let mismatched = run(
        &rust.capability.shared(),
        &[dict(&widening_args(
            "caller",
            "src/a.rs",
            "src/b.rs",
            Some("export"),
        ))],
    );
    assert!(mismatched.is_err(), "export is not a Rust level");
    let unknown = run(
        &rust.capability.shared(),
        &[dict(&widening_args(
            "caller",
            "src/a.rs",
            "src/b.rs",
            Some("pub(in x)"),
        ))],
    );
    assert!(unknown.is_err(), "unknown levels are rejected, not ignored");
    let ts = Fixture::new(&[
        (
            "src/a.ts",
            "function helper(): number {\n  return 1\n}\n\nexport function caller(): number {\n  return helper()\n}\n",
        ),
        ("src/b.ts", "export const B = 1\n"),
    ]);
    let crate_level = run(
        &ts.capability.shared(),
        &[dict(&widening_args(
            "caller",
            "src/a.ts",
            "src/b.ts",
            Some("pub(crate)"),
        ))],
    );
    assert!(crate_level.is_err(), "pub(crate) is not a TypeScript level");
}

#[test]
fn ts_widening_exports_exactly_the_blocking_declarations() {
    let fixture = Fixture::new(&[
        (
            "src/a.ts",
            "function helper(): number {\n  return 1\n}\n\nfunction untouched(): number {\n  return 2\n}\n\nexport function caller(): number {\n  return helper()\n}\n",
        ),
        ("src/b.ts", "export const B = 1\n"),
    ]);
    fixture.refused(
        "visibility_required",
        &widening_args("caller", "src/a.ts", "src/b.ts", None),
    );
    let result = fixture.move_symbol(&widening_args(
        "caller",
        "src/a.ts",
        "src/b.ts",
        Some("export"),
    ));
    assert_eq!(tag(&result), "applied", "{result:?}");
    assert_eq!(
        widened(&result),
        [(
            "src/a.ts".to_string(),
            1,
            "unexported".to_string(),
            "export".to_string()
        )]
    );
    assert_eq!(
        fixture.read("src/a.ts"),
        "export function helper(): number {\n  return 1\n}\n\nfunction untouched(): number {\n  return 2\n}\n"
    );
    assert!(fixture
        .read("src/b.ts")
        .contains("import { helper } from \"./a\""));
}

#[test]
fn ts_widening_exports_a_moved_item_its_source_still_uses() {
    let fixture = Fixture::new(&[
        (
            "src/a.ts",
            "function helper(): number {\n  return 1\n}\n\nexport function caller(): number {\n  return helper()\n}\n",
        ),
        ("src/b.ts", "export const B = 1\n"),
    ]);
    let result = fixture.move_symbol(&widening_args(
        "helper",
        "src/a.ts",
        "src/b.ts",
        Some("export"),
    ));
    assert_eq!(tag(&result), "applied", "{result:?}");
    assert_eq!(
        widened(&result),
        [(
            "src/a.ts".to_string(),
            1,
            "unexported".to_string(),
            "export".to_string()
        )]
    );
    assert!(fixture
        .read("src/b.ts")
        .contains("export function helper(): number {"));
}

#[test]
fn rust_widened_item_in_a_new_module_gets_only_the_module_visibility_it_needs() {
    // Users in the item's former sibling: `pub(super)` reaches them, and the
    // private `mod` line the new module gets is already visible there.
    let near = sibling_modules_fixture();
    let result = near.move_symbol(&widening_args(
        "helper",
        "src/a.rs",
        "src/c.rs",
        Some("pub(crate)"),
    ));
    assert_eq!(tag(&result), "applied", "{result:?}");
    assert_eq!(
        widened(&result),
        [(
            "src/a.rs".to_string(),
            1,
            "private".to_string(),
            "pub(super)".to_string()
        )]
    );
    assert_eq!(near.read("src/lib.rs"), "pub mod a;\npub mod b;\nmod c;\n");

    // Users two levels away need `pub(crate)`, and the module line carries it
    // and is reported.
    let far = Fixture::new(&[
        KIT,
        ("src/lib.rs", "pub mod a;\npub mod b;\n"),
        ("src/a/mod.rs", "pub mod model;\n"),
        (
            "src/a/model.rs",
            "fn helper() -> u8 {\n    1\n}\n\npub fn caller() -> u8 {\n    helper()\n}\n",
        ),
        ("src/b/mod.rs", "pub fn other() {}\n"),
    ]);
    let refused = far.refused(
        "visibility_required",
        &widening_args(
            "helper",
            "src/a/model.rs",
            "src/b/moved.rs",
            Some("pub(super)"),
        ),
    );
    assert_eq!(s(field(&refused, "required_visibility")), "pub(crate)");
    let result = far.move_symbol(&widening_args(
        "helper",
        "src/a/model.rs",
        "src/b/moved.rs",
        Some("pub(crate)"),
    ));
    assert_eq!(tag(&result), "applied", "{result:?}");
    assert_eq!(
        widened(&result),
        [
            (
                "src/b/mod.rs".to_string(),
                1,
                "private".to_string(),
                "pub(crate)".to_string()
            ),
            (
                "src/a/model.rs".to_string(),
                1,
                "private".to_string(),
                "pub(crate)".to_string()
            ),
        ]
    );
    assert!(far.read("src/b/mod.rs").contains("pub(crate) mod moved;"));
}

#[test]
fn rust_restricted_visibility_is_not_crate_wide() {
    let files = |helper: &'static str| -> [(&'static str, String); 6] {
        [
            (
                "Cargo.toml",
                "[package]\nname = \"kit\"\nversion = \"0.1.0\"\n".to_string(),
            ),
            ("src/lib.rs", "pub mod a;\npub mod b;\n".to_string()),
            (
                "src/a/mod.rs",
                "pub mod model;\npub mod sibling;\n".to_string(),
            ),
            (
                "src/a/model.rs",
                format!(
                    "{helper} fn helper() -> u8 {{\n    1\n}}\n\npub fn caller() -> u8 {{\n    helper()\n}}\n"
                ),
            ),
            ("src/a/sibling.rs", "pub fn other() {}\n".to_string()),
            ("src/b/mod.rs", "pub mod moved;\n".to_string()),
        ]
    };
    let fixture = |helper: &'static str| {
        let mut all: Vec<(String, String)> = files(helper)
            .into_iter()
            .map(|(p, b)| (p.to_string(), b))
            .collect();
        all.push((
            "src/b/moved.rs".to_string(),
            "pub fn other() {}\n".to_string(),
        ));
        let refs: Vec<(&str, &str)> = all.iter().map(|(p, b)| (p.as_str(), b.as_str())).collect();
        Fixture::new(&refs)
    };

    // `pub(super)` reaches `a` and its children, not `b::moved`.
    let hidden = fixture("pub(super)");
    let refused = hidden.refused(
        "visibility_required",
        &widening_args("caller", "src/a/model.rs", "src/b/moved.rs", None),
    );
    assert_eq!(s(field(&refused, "required_visibility")), "pub(crate)");
    let result = hidden.move_symbol(&widening_args(
        "caller",
        "src/a/model.rs",
        "src/b/moved.rs",
        Some("pub(crate)"),
    ));
    assert_eq!(tag(&result), "applied", "{result:?}");
    assert_eq!(
        widened(&result),
        [(
            "src/a/model.rs".to_string(),
            1,
            "pub(super)".to_string(),
            "pub(crate)".to_string()
        )]
    );
    assert!(hidden
        .read("src/a/model.rs")
        .starts_with("pub(crate) fn helper()"));

    // The destination inside the region sees it already: no refusal, no change.
    let inside = fixture("pub(super)");
    let result = inside.mv("caller", "src/a/model.rs", "src/a/sibling.rs");
    assert_eq!(tag(&result), "applied", "{result:?}");
    assert!(inside
        .read("src/a/model.rs")
        .starts_with("pub(super) fn helper()"));

    // A path-restricted modifier is never rewritten, whatever the allowance.
    let pathed = fixture("pub(in crate::a)");
    pathed.refused(
        "visibility_required",
        &widening_args("caller", "src/a/model.rs", "src/b/moved.rs", Some("pub")),
    );
}

#[test]
fn rust_widening_leaves_a_same_named_member_of_another_type_alone() {
    // The moved function flushes a `Stdout`; the private `flush` belongs to
    // `Buffer`, which the function never names.
    let unrelated = Fixture::new(&[
        KIT,
        ("src/lib.rs", "pub mod a;\npub mod b;\n"),
        (
            "src/a.rs",
            "pub struct Buffer;\n\nimpl Buffer {\n    fn flush(&self) {}\n}\n\npub fn run(out: &mut std::io::Stdout) {\n    use std::io::Write;\n    out.flush().unwrap();\n}\n",
        ),
        ("src/b.rs", "pub fn other() {}\n"),
    ]);
    unrelated.refused(
        "visibility_required",
        &widening_args("run", "src/a.rs", "src/b.rs", Some("pub")),
    );

    // Two owners declare a private `flush`: the name does not pick one.
    let twins = Fixture::new(&[
        KIT,
        ("src/lib.rs", "pub mod a;\npub mod b;\n"),
        (
            "src/a.rs",
            "pub struct One;\npub struct Two;\n\nimpl One {\n    fn flush(&self) {}\n}\n\nimpl Two {\n    fn flush(&self) {}\n}\n\npub fn run(one: &One) {\n    one.flush();\n}\n",
        ),
        ("src/b.rs", "pub fn other() {}\n"),
    ]);
    twins.refused(
        "visibility_required",
        &widening_args("run", "src/a.rs", "src/b.rs", Some("pub")),
    );
}

#[test]
fn rust_widening_requires_the_receiver_to_be_the_owning_type() {
    // The function names `Buffer` and calls `flush`, but on a `Stdout`.
    let fixture = Fixture::new(&[
        KIT,
        ("src/lib.rs", "pub mod a;\npub mod b;\n"),
        (
            "src/a.rs",
            "pub struct Buffer;\n\nimpl Buffer {\n    fn flush(&self) {}\n}\n\npub fn run(_buf: &Buffer, out: &mut std::io::Stdout) {\n    use std::io::Write;\n    out.flush().unwrap();\n}\n",
        ),
        ("src/b.rs", "pub fn other() {}\n"),
    ]);
    fixture.refused(
        "visibility_required",
        &widening_args("run", "src/a.rs", "src/b.rs", Some("pub")),
    );
}

#[test]
fn rust_widening_respects_receiver_lexical_scope() {
    for body in [
        "pub fn run(out: &mut std::io::Stdout) {\n    fn helper(out: &Buffer) {}\n    use std::io::Write;\n    out.flush().unwrap();\n}\n",
        "pub fn run(out: &Buffer) {\n    fn helper(out: &mut std::io::Stdout) {\n        use std::io::Write;\n        out.flush().unwrap();\n    }\n}\n",
        "pub fn run<Buffer: std::io::Write>(out: &mut Buffer) {\n    out.flush().unwrap();\n}\n",
        "pub fn run(out: &crate::other::Buffer) {\n    out.flush();\n}\n",
        "pub fn run() {\n    use crate::other::*;\n    let _ = Buffer { flush: 1 };\n}\n",
        "pub fn run(out: &Buffer, other: &crate::other::Buffer) {\n    let r#out = other;\n    out.flush();\n}\n",
        "pub fn run(out: &Buffer, other: &crate::other::Buffer) {\n    macro_rules! bind { ($name:ident, $value:expr) => { let $name = $value; }; }\n    bind!(out, other);\n    out.flush();\n}\n",
        "pub fn run(out: &Buffer, other: &crate::other::Buffer) {\n    macro_rules! assert { ($name:ident, $value:expr) => { let $name = $value; }; }\n    assert!(out, other);\n    out.flush();\n}\n",
        "macro_rules! install { ($d:tt) => { macro_rules! assert { ($d name:ident, $d value:expr) => { let $d name = $d value; }; } }; }\ninstall!($);\npub fn run(out: &Buffer, other: &crate::other::Buffer) {\n    assert!(out, other);\n    out.flush();\n}\n",
        "pub fn run() {\n    #[derive(binding_macro::InstallBuffer)]\n    struct Marker;\n    let _ = Buffer { flush: 1 };\n}\n",
        "pub fn run() {\n    #[cfg_attr(all(), binding_macro::install_buffer)]\n    struct Marker;\n    let _ = Buffer { flush: 1 };\n}\n",
        "pub fn run() {\n    #[cfg_attr(all(), derive(binding_macro::InstallBuffer))]\n    struct Marker;\n    let _ = Buffer { flush: 1 };\n}\n",
        "pub fn run() {\n    #[cfg_attr(all(), cfg_attr(all(), binding_macro::install_buffer))]\n    struct Marker;\n    let _ = Buffer { flush: 1 };\n}\n",
    ] {
        let source =
            format!("pub struct Buffer;\n\nimpl Buffer {{\n    fn flush(&self) {{}}\n}}\n\n{body}");
        let fixture = Fixture::new(&[
            KIT,
            ("src/lib.rs", "pub mod a;\npub mod b;\npub mod other;\n"),
            ("src/a.rs", &source),
            ("src/b.rs", "pub fn other() {}\n"),
            (
                "src/other.rs",
                "pub struct Buffer { pub flush: u8 }\nimpl Buffer { pub fn flush(&self) {} }\n",
            ),
        ]);
        fixture.refused(
            "visibility_required",
            &widening_args("run", "src/a.rs", "src/b.rs", Some("pub")),
        );
    }
}

#[test]
fn rust_widening_keeps_proven_receivers_in_their_own_scope() {
    for body in [
        "pub fn run(out: &Buffer) {\n    fn helper(out: &mut std::io::Stdout) {}\n    out.flush();\n}\n",
        "pub fn run() {\n    fn helper(out: &Buffer) { out.flush(); }\n}\n",
        "pub fn run(out: &Buffer) {\n    { let out = 3; let _ = out; }\n    out.flush();\n}\n",
        "pub fn run(out: &Buffer) {\n    assert!(true);\n    out.flush();\n}\n",
        "pub fn run(out: &Buffer) {\n    use std::io::Write;\n    std::assert!(true);\n    out.flush();\n}\n",
        "pub fn run(out: &Buffer) {\n    #[derive(Debug, Clone)]\n    struct Marker;\n    out.flush();\n}\n",
        "pub fn run(out: &Buffer) {\n    #[cfg_attr(all(), derive(Debug, Clone))]\n    struct Marker;\n    out.flush();\n}\n",
    ] {
        let source =
            format!("pub struct Buffer;\n\nimpl Buffer {{\n    fn flush(&self) {{}}\n}}\n\n{body}");
        let fixture = Fixture::new(&[
            KIT,
            ("src/lib.rs", "pub mod a;\npub mod b;\n"),
            ("src/a.rs", &source),
            ("src/b.rs", "pub fn other() {}\n"),
        ]);
        let result =
            fixture.move_symbol(&widening_args("run", "src/a.rs", "src/b.rs", Some("pub")));
        assert_eq!(tag(&result), "applied", "{result:?}");
        assert_eq!(
            widened(&result),
            [(
                "src/a.rs".to_string(),
                4,
                "private".to_string(),
                "pub(super)".to_string()
            )],
        );
    }
}
