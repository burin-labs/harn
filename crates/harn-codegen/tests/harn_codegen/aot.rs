//! Ahead-of-time object emission tests.
//!
//! We assert the backend produces a well-formed host object exporting the
//! expected symbol. We deliberately do *not* invoke a system linker — that
//! would make the test depend on an external toolchain. The object's format
//! magic plus the presence of the export symbol is enough to prove the backend
//! ran end to end.

use harn_codegen::compile_named_object;

#[test]
fn emits_object_with_expected_symbol() {
    let artifact =
        compile_named_object("fn add(a: int, b: int) -> int { return a + b }", "add").unwrap();

    assert_eq!(artifact.symbol, "harn_scalar_add");
    assert!(!artifact.bytes.is_empty(), "object should not be empty");

    // The export symbol name appears in the object's string/symbol table.
    let needle = artifact.symbol.as_bytes();
    let found = artifact
        .bytes
        .windows(needle.len())
        .any(|window| window == needle);
    assert!(
        found,
        "exported symbol `{}` not present in object",
        artifact.symbol
    );

    assert!(
        has_known_object_magic(&artifact.bytes),
        "object does not start with a recognised format magic"
    );
}

/// Recognise the leading magic for the host object formats Cranelift emits.
fn has_known_object_magic(bytes: &[u8]) -> bool {
    const ELF: &[u8] = &[0x7f, b'E', b'L', b'F'];
    const MACHO64_LE: &[u8] = &[0xcf, 0xfa, 0xed, 0xfe];
    const MACHO64_BE: &[u8] = &[0xfe, 0xed, 0xfa, 0xcf];
    // COFF (Windows) object files begin with a 2-byte machine type rather than
    // a fixed magic; accept the common x86-64/aarch64 machine words too.
    const COFF_X64: &[u8] = &[0x64, 0x86];
    const COFF_ARM64: &[u8] = &[0xaa, 0x64];

    [ELF, MACHO64_LE, MACHO64_BE, COFF_X64, COFF_ARM64]
        .iter()
        .any(|magic| bytes.starts_with(magic))
}
