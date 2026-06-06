//! Scalar value and type model shared by the verifier, the reference
//! evaluator, and the Cranelift backends.
//!
//! The native compiler only handles Harn's three unboxed scalar shapes —
//! `int` (`i64`), `float` (`f64`), and `bool`. Everything heap-allocated or
//! runtime-tagged (strings, lists, dicts, nil, closures, host handles) is out
//! of scope by construction, which is exactly what lets the generated code use
//! flat machine registers with no tag checks.

use std::fmt;

/// The static type of a scalar operand stack slot, local, parameter, or
/// return value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScalarType {
    Int,
    Float,
    Bool,
}

impl ScalarType {
    /// The Harn surface-syntax name for this type, as it appears in a
    /// parameter annotation.
    #[must_use]
    pub const fn harn_name(self) -> &'static str {
        match self {
            Self::Int => "int",
            Self::Float => "float",
            Self::Bool => "bool",
        }
    }

    /// Parse a Harn `TypeExpr::Named` payload into a scalar type, if it names
    /// one of the three unboxed scalars.
    #[must_use]
    pub fn from_harn_name(name: &str) -> Option<Self> {
        match name {
            "int" => Some(Self::Int),
            "float" => Some(Self::Float),
            "bool" => Some(Self::Bool),
            _ => None,
        }
    }
}

impl fmt::Display for ScalarType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.harn_name())
    }
}

/// A concrete scalar value. Used as the argument/return marshalling type for
/// both the reference evaluator and the JIT-compiled function.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ScalarValue {
    Int(i64),
    Float(f64),
    Bool(bool),
}

impl ScalarValue {
    #[must_use]
    pub const fn ty(self) -> ScalarType {
        match self {
            Self::Int(_) => ScalarType::Int,
            Self::Float(_) => ScalarType::Float,
            Self::Bool(_) => ScalarType::Bool,
        }
    }

    /// Encode the value as the raw 64-bit pattern used by the uniform native
    /// calling convention: `int` keeps its two's-complement bits, `float`
    /// uses IEEE-754 bits, and `bool` is `0`/`1`.
    #[must_use]
    pub const fn to_bits(self) -> u64 {
        match self {
            Self::Int(n) => n as u64,
            Self::Float(f) => f.to_bits(),
            Self::Bool(b) => b as u64,
        }
    }

    /// Decode a raw 64-bit pattern back into a typed value given the slot's
    /// statically known type. Inverse of [`ScalarValue::to_bits`].
    #[must_use]
    pub const fn from_bits(ty: ScalarType, bits: u64) -> Self {
        match ty {
            ScalarType::Int => Self::Int(bits as i64),
            ScalarType::Float => Self::Float(f64::from_bits(bits)),
            ScalarType::Bool => Self::Bool(bits & 1 != 0),
        }
    }
}

impl fmt::Display for ScalarValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Int(n) => write!(f, "{n}"),
            Self::Float(x) => write!(f, "{x}"),
            Self::Bool(b) => write!(f, "{b}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ScalarType, ScalarValue};

    #[test]
    fn bits_roundtrip() {
        for value in [
            ScalarValue::Int(0),
            ScalarValue::Int(-42),
            ScalarValue::Int(i64::MIN),
            ScalarValue::Int(i64::MAX),
            ScalarValue::Float(3.5),
            ScalarValue::Float(-0.0),
            ScalarValue::Bool(true),
            ScalarValue::Bool(false),
        ] {
            assert_eq!(ScalarValue::from_bits(value.ty(), value.to_bits()), value);
        }
    }

    #[test]
    fn nan_bits_survive_roundtrip() {
        let bits = ScalarValue::Float(f64::NAN).to_bits();
        match ScalarValue::from_bits(ScalarType::Float, bits) {
            ScalarValue::Float(x) => assert!(x.is_nan()),
            other => panic!("expected float, got {other:?}"),
        }
    }

    #[test]
    fn scalar_type_names() {
        assert_eq!(ScalarType::from_harn_name("int"), Some(ScalarType::Int));
        assert_eq!(ScalarType::from_harn_name("float"), Some(ScalarType::Float));
        assert_eq!(ScalarType::from_harn_name("bool"), Some(ScalarType::Bool));
        assert_eq!(ScalarType::from_harn_name("Widget"), None);
        assert_eq!(ScalarType::Int.harn_name(), "int");
    }
}
