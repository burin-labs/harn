//! Closed builtin vocabulary implemented by Portable Kernel v1.
//!
//! Artifact verification and execution both resolve through this enum. Adding
//! a portable builtin is therefore one semantic decision, not two string
//! tables that can drift.

macro_rules! define_portable_builtins {
    ($($variant:ident => ($name:literal, $bytes:literal)),+ $(,)?) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub(crate) enum PortableBuiltin { $($variant),+ }

        impl PortableBuiltin {
            pub(crate) const ALL: &'static [(&'static str, Self)] = &[
                $(($name, Self::$variant)),+
            ];

            pub(crate) const fn from_name(name: &str) -> Option<Self> {
                match name.as_bytes() {
                    $($bytes => Some(Self::$variant),)+
                    _ => None,
                }
            }
        }
    };
}

define_portable_builtins! {
    Len => ("len", b"len"),
    Count => ("count", b"count"),
    String => ("string", b"string"),
    MakeStruct => ("__make_struct", b"__make_struct"),
    AssertList => ("__assert_list", b"__assert_list"),
}
