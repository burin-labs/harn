use std::fmt;

/// Compact, deterministic identifier for a builtin name.
///
/// The VM keeps name-keyed builtin maps as the authoritative registry for
/// dynamic calls and diagnostics. Hot direct-call bytecode can use this ID to
/// hit the side index without repeating string-keyed map lookups.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BuiltinId(u64);

impl BuiltinId {
    pub const fn from_name(name: &str) -> Self {
        // FNV-1a 64-bit. Stable across processes and cheap enough for compile
        // and registration time.
        let bytes = name.as_bytes();
        let mut hash = 0xcbf29ce484222325u64;
        let mut i = 0;
        while i < bytes.len() {
            hash ^= bytes[i] as u64;
            hash = hash.wrapping_mul(0x100000001b3);
            i += 1;
        }
        Self(hash)
    }

    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u64 {
        self.0
    }
}

impl fmt::Display for BuiltinId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:#018x}", self.0)
    }
}
