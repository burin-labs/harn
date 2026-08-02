//! Stable opcode vocabulary shared by every Harn execution target.
//!
//! This is the bytecode ABI's single schema. Numeric discriminants and operand
//! layouts are versioned artifact data, not implementation details. Consumers
//! must use [`Op::operands`] instead of maintaining byte-width tables.

/// One encoded operand in a Harn bytecode instruction.
///
/// The width and semantic role live together so artifact verification can
/// validate indices and jump targets without another opcode table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperandKind {
    ImmediateU8,
    ImmediateU16,
    BuiltinIdU64,
    ConstantU16,
    StringConstantU16,
    LocalU16,
    FunctionU16,
    JumpU16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Portability {
    PortableV1,
    UnsupportedV1,
}

impl OperandKind {
    pub const fn width(self) -> usize {
        match self {
            Self::ImmediateU8 => 1,
            Self::ImmediateU16
            | Self::ConstantU16
            | Self::StringConstantU16
            | Self::LocalU16
            | Self::FunctionU16
            | Self::JumpU16 => 2,
            Self::BuiltinIdU64 => 8,
        }
    }

    const fn abi_tag(self) -> u8 {
        match self {
            Self::ImmediateU8 => 0,
            Self::ImmediateU16 => 1,
            Self::BuiltinIdU64 => 2,
            Self::ConstantU16 => 3,
            Self::LocalU16 => 4,
            Self::FunctionU16 => 5,
            Self::JumpU16 => 6,
            Self::StringConstantU16 => 7,
        }
    }
}

macro_rules! define_opcodes {
    ($($name:ident = $byte:literal => [$($operand:ident),* $(,)?]),+ $(,)?) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
        #[repr(u8)]
        pub enum Op { $($name = $byte),+ }

        impl Op {
            pub const ALL: &'static [Self] = &[$(Self::$name),+];
            pub const COUNT: usize = Self::ALL.len();

            #[inline]
            pub fn from_byte(byte: u8) -> Option<Self> {
                Self::ALL.get(byte as usize).copied()
            }

            pub const fn name(self) -> &'static str {
                match self { $(Self::$name => stringify!($name)),+ }
            }

            pub const fn operands(self) -> &'static [OperandKind] {
                match self {
                    $(Self::$name => &[$(OperandKind::$operand),*]),+
                }
            }

            pub const fn instruction_len(self) -> usize {
                let operands = self.operands();
                let mut index = 0;
                let mut width = 1;
                while index < operands.len() {
                    width += operands[index].width();
                    index += 1;
                }
                width
            }
        }
    };
}

define_opcodes! {
    Constant = 0 => [ConstantU16],
    Nil = 1 => [],
    True = 2 => [],
    False = 3 => [],
    RootHarness = 4 => [],
    GetVar = 5 => [StringConstantU16],
    DefLet = 6 => [StringConstantU16],
    DefVar = 7 => [StringConstantU16],
    DefCell = 8 => [StringConstantU16],
    SetVar = 9 => [StringConstantU16],
    PushScope = 10 => [],
    PopScope = 11 => [],
    Add = 12 => [],
    Sub = 13 => [],
    Mul = 14 => [],
    Div = 15 => [],
    Mod = 16 => [],
    Pow = 17 => [],
    Negate = 18 => [],
    Equal = 19 => [],
    NotEqual = 20 => [],
    Less = 21 => [],
    Greater = 22 => [],
    LessEqual = 23 => [],
    GreaterEqual = 24 => [],
    Not = 25 => [],
    Jump = 26 => [JumpU16],
    JumpIfFalse = 27 => [JumpU16],
    JumpIfTrue = 28 => [JumpU16],
    Pop = 29 => [],
    Call = 30 => [ImmediateU8],
    TailCall = 31 => [ImmediateU8],
    Return = 32 => [],
    Closure = 33 => [FunctionU16],
    BuildList = 34 => [ImmediateU16],
    BuildDict = 35 => [ImmediateU16],
    Subscript = 36 => [],
    SubscriptOpt = 37 => [],
    Slice = 38 => [],
    GetProperty = 39 => [StringConstantU16],
    GetPropertyOpt = 40 => [StringConstantU16],
    SetProperty = 41 => [StringConstantU16],
    SetSubscript = 42 => [StringConstantU16],
    SetLocalSlotProperty = 43 => [StringConstantU16, LocalU16],
    SetLocalSlotSubscript = 44 => [LocalU16],
    MethodCall = 45 => [StringConstantU16, ImmediateU8],
    MethodCallOpt = 46 => [StringConstantU16, ImmediateU8],
    Concat = 47 => [ImmediateU16],
    IterInit = 48 => [],
    IterNext = 49 => [JumpU16],
    Pipe = 50 => [],
    Throw = 51 => [],
    TryCatchSetup = 52 => [JumpU16, StringConstantU16],
    PopHandler = 53 => [],
    Parallel = 54 => [],
    ParallelMap = 55 => [],
    ParallelMapStream = 56 => [],
    ParallelSettle = 57 => [],
    Spawn = 58 => [],
    SyncMutexEnter = 59 => [],
    SyncMutexEnterKeyed = 60 => [],
    TaskScopeEnter = 61 => [],
    TaskScopeExit = 62 => [],
    Import = 63 => [StringConstantU16],
    SelectiveImport = 64 => [StringConstantU16, StringConstantU16],
    NamespaceImport = 65 => [StringConstantU16, StringConstantU16],
    DeadlineSetup = 66 => [],
    DeadlineEnd = 67 => [],
    BuildEnum = 68 => [StringConstantU16, StringConstantU16, ImmediateU16],
    MatchEnum = 69 => [StringConstantU16, StringConstantU16],
    PopIterator = 70 => [],
    GetArgc = 71 => [],
    CheckType = 72 => [StringConstantU16, StringConstantU16],
    TryUnwrap = 73 => [],
    TryWrapOk = 74 => [],
    CallSpread = 75 => [],
    CallBuiltin = 76 => [BuiltinIdU64, StringConstantU16, ImmediateU8],
    CallBuiltinSpread = 77 => [BuiltinIdU64, StringConstantU16],
    MethodCallSpread = 78 => [StringConstantU16],
    Dup = 79 => [],
    Swap = 80 => [],
    Contains = 81 => [],
    AddInt = 82 => [],
    SubInt = 83 => [],
    MulInt = 84 => [],
    DivInt = 85 => [],
    ModInt = 86 => [],
    AddFloat = 87 => [],
    SubFloat = 88 => [],
    MulFloat = 89 => [],
    DivFloat = 90 => [],
    ModFloat = 91 => [],
    EqualInt = 92 => [],
    NotEqualInt = 93 => [],
    LessInt = 94 => [],
    GreaterInt = 95 => [],
    LessEqualInt = 96 => [],
    GreaterEqualInt = 97 => [],
    EqualFloat = 98 => [],
    NotEqualFloat = 99 => [],
    LessFloat = 100 => [],
    GreaterFloat = 101 => [],
    LessEqualFloat = 102 => [],
    GreaterEqualFloat = 103 => [],
    EqualBool = 104 => [],
    NotEqualBool = 105 => [],
    EqualString = 106 => [],
    NotEqualString = 107 => [],
    Yield = 108 => [],
    GetLocalSlot = 109 => [LocalU16],
    DefLocalSlot = 110 => [LocalU16],
    SetLocalSlot = 111 => [LocalU16],
    ConcatAssignLocal = 112 => [LocalU16],
}

impl Op {
    /// Whether Portable Kernel v1 has an explicit execution arm for this
    /// opcode. Artifact validation uses this closed classification so an opcode
    /// addition cannot become browser-executable by omission.
    pub const fn portability(self) -> Portability {
        match self {
            Self::Constant
            | Self::Nil
            | Self::True
            | Self::False
            | Self::RootHarness
            | Self::GetVar
            | Self::DefLet
            | Self::DefVar
            | Self::DefCell
            | Self::SetVar
            | Self::PushScope
            | Self::PopScope
            | Self::Add
            | Self::Sub
            | Self::Mul
            | Self::Div
            | Self::Mod
            | Self::Pow
            | Self::Negate
            | Self::Equal
            | Self::NotEqual
            | Self::Less
            | Self::Greater
            | Self::LessEqual
            | Self::GreaterEqual
            | Self::Not
            | Self::Jump
            | Self::JumpIfFalse
            | Self::JumpIfTrue
            | Self::Pop
            | Self::Call
            | Self::TailCall
            | Self::Return
            | Self::Closure
            | Self::BuildList
            | Self::BuildDict
            | Self::Subscript
            | Self::SubscriptOpt
            | Self::Slice
            | Self::GetProperty
            | Self::GetPropertyOpt
            | Self::MethodCall
            | Self::MethodCallOpt
            | Self::Concat
            | Self::Throw
            | Self::TryCatchSetup
            | Self::PopHandler
            | Self::GetArgc
            | Self::CallBuiltin
            | Self::CallBuiltinSpread
            | Self::Dup
            | Self::Swap
            | Self::Contains
            | Self::AddInt
            | Self::SubInt
            | Self::MulInt
            | Self::DivInt
            | Self::ModInt
            | Self::AddFloat
            | Self::SubFloat
            | Self::MulFloat
            | Self::DivFloat
            | Self::ModFloat
            | Self::EqualInt
            | Self::NotEqualInt
            | Self::LessInt
            | Self::GreaterInt
            | Self::LessEqualInt
            | Self::GreaterEqualInt
            | Self::EqualFloat
            | Self::NotEqualFloat
            | Self::LessFloat
            | Self::GreaterFloat
            | Self::LessEqualFloat
            | Self::GreaterEqualFloat
            | Self::EqualBool
            | Self::NotEqualBool
            | Self::EqualString
            | Self::NotEqualString
            | Self::GetLocalSlot
            | Self::DefLocalSlot
            | Self::SetLocalSlot
            | Self::ConcatAssignLocal => Portability::PortableV1,

            Self::SetProperty
            | Self::SetSubscript
            | Self::SetLocalSlotProperty
            | Self::SetLocalSlotSubscript
            | Self::IterInit
            | Self::IterNext
            | Self::Pipe
            | Self::Parallel
            | Self::ParallelMap
            | Self::ParallelMapStream
            | Self::ParallelSettle
            | Self::Spawn
            | Self::SyncMutexEnter
            | Self::SyncMutexEnterKeyed
            | Self::TaskScopeEnter
            | Self::TaskScopeExit
            | Self::Import
            | Self::SelectiveImport
            | Self::NamespaceImport
            | Self::DeadlineSetup
            | Self::DeadlineEnd
            | Self::BuildEnum
            | Self::MatchEnum
            | Self::PopIterator
            | Self::CheckType
            | Self::TryUnwrap
            | Self::TryWrapOk
            | Self::CallSpread
            | Self::MethodCallSpread
            | Self::Yield => Portability::UnsupportedV1,
        }
    }

    pub const fn is_portable_v1(self) -> bool {
        matches!(self.portability(), Portability::PortableV1)
    }
}

/// Artifact format version whose golden opcode fingerprint is pinned below.
pub const OPCODE_ABI_ARTIFACT_VERSION: u16 = 1;

/// Golden BLAKE3 digest of opcode bytes, names, and operand-role tags for v1.
///
/// Changing the schema requires an intentional artifact-version bump and a new
/// named fingerprint rather than silently rewriting existing bytecode.
pub const OPCODE_ABI_FINGERPRINT_V1: [u8; 32] = [
    0x76, 0x1f, 0x93, 0x67, 0xa5, 0x69, 0xd4, 0x18, 0x8b, 0x00, 0xf7, 0xaf, 0x36, 0xe5, 0x51, 0x20,
    0xcb, 0xb6, 0xed, 0x92, 0xf3, 0x47, 0x95, 0xa0, 0x59, 0x3d, 0x02, 0x9e, 0x57, 0xbe, 0x20, 0x0e,
];

/// Compute the fingerprint of the compiled opcode schema.
pub fn opcode_abi_fingerprint() -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    for op in Op::ALL {
        hasher.update(&[*op as u8]);
        hasher.update(op.name().as_bytes());
        hasher.update(&[0]);
        for operand in op.operands() {
            hasher.update(&[operand.abi_tag()]);
        }
        hasher.update(&[0xff]);
    }
    *hasher.finalize().as_bytes()
}

#[cfg(test)]
mod tests {
    use super::{
        opcode_abi_fingerprint, Op, OPCODE_ABI_ARTIFACT_VERSION, OPCODE_ABI_FINGERPRINT_V1,
    };

    #[test]
    fn byte_mapping_is_explicit_dense_and_stable() {
        for (byte, op) in Op::ALL.iter().copied().enumerate() {
            assert_eq!(Op::from_byte(byte as u8), Some(op));
            assert_eq!(op as usize, byte);
        }
        assert_eq!(Op::from_byte(Op::COUNT as u8), None);
    }

    #[test]
    fn opcode_schema_matches_artifact_v1_golden() {
        assert_eq!(OPCODE_ABI_ARTIFACT_VERSION, crate::ARTIFACT_VERSION);
        assert_eq!(opcode_abi_fingerprint(), OPCODE_ABI_FINGERPRINT_V1);
    }
}
