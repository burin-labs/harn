//! Grammars whose parse tables the build script compressed. See `build.rs`.

use std::io::Read;
use std::sync::OnceLock;

use tree_sitter::Language;
use tree_sitter_language::LanguageFn;

pub(crate) struct Packed {
    name: &'static str,
    /// The grammar's tables, zstd-compressed and concatenated in the order
    /// `tables` reports them.
    blob: &'static [u8],
    /// Each table's size in bytes on the build host.
    sizes: &'static [usize],
    /// Writes each table's address and size in this binary, returning the
    /// table count.
    tables: unsafe extern "C" fn(*mut *mut u8, *mut usize, usize) -> usize,
    entry: unsafe extern "C" fn() -> *const (),
    ready: OnceLock<()>,
}

impl Packed {
    pub(crate) fn language(&'static self) -> Language {
        self.ready.get_or_init(|| self.unpack());
        // SAFETY: `entry` returns the grammar's `TSLanguage`, whose tables
        // `unpack` has filled; nothing writes them again.
        Language::new(unsafe { LanguageFn::from_raw(self.entry) })
    }

    fn unpack(&self) {
        let count = self.sizes.len();
        let mut tables = vec![std::ptr::null_mut::<u8>(); count];
        let mut sizes = vec![0usize; count];
        // SAFETY: both buffers hold `count` entries, the capacity passed.
        let reported = unsafe { (self.tables)(tables.as_mut_ptr(), sizes.as_mut_ptr(), count) };
        assert!(
            reported == count && sizes == self.sizes,
            "grammar {}: this target lays out its tables differently from the build host",
            self.name
        );
        let mut decoder =
            zstd::stream::read::Decoder::with_buffer(self.blob).expect("start table decoder");
        for (&table, &size) in tables.iter().zip(&sizes) {
            // SAFETY: `table` is a zero-filled, writable static array of
            // `size` bytes that no parser has read yet: `ready` runs this
            // once, before any `Language` for the grammar exists.
            let table = unsafe { std::slice::from_raw_parts_mut(table, size) };
            decoder
                .read_exact(table)
                .unwrap_or_else(|e| panic!("grammar {}: unpacking tables: {e}", self.name));
        }
        let mut rest = [0u8; 1];
        assert_eq!(
            decoder.read(&mut rest).ok(),
            Some(0),
            "grammar {}: packed tables are longer than the grammar's",
            self.name
        );
    }
}

include!(concat!(env!("OUT_DIR"), "/packed_grammars.rs"));
