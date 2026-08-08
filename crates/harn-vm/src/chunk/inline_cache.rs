//! Native runtime feedback attached to canonical portable bytecode.
//!
//! Portable artifacts retain only cache-slot locations. Each native VM fills
//! these process-local entries independently, so cached behavior never leaks
//! into serialization or across isolates.

use std::sync::Arc;

use crate::harness::HarnessKind;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InlineCacheEntry {
    Empty,
    Property {
        name_idx: u16,
        target: PropertyCacheTarget,
    },
    Method {
        name_idx: u16,
        argc: usize,
        target: MethodCacheTarget,
    },
    AdaptiveBinary {
        op: AdaptiveBinaryOp,
        state: AdaptiveBinaryState,
    },
    DirectCall {
        state: DirectCallState,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AdaptiveBinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Equal,
    NotEqual,
    Less,
    Greater,
    LessEqual,
    GreaterEqual,
}

/// Adaptive-binary IC state. All fields are scalar `Copy`, which lets the
/// specialization path inspect the state without cloning its entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AdaptiveBinaryState {
    Warmup {
        shape: BinaryShape,
        hits: u8,
    },
    Specialized {
        shape: BinaryShape,
        hits: u64,
        misses: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BinaryShape {
    Int,
    Float,
    Bool,
    String,
}

#[derive(Debug, Clone)]
pub(crate) enum DirectCallState {
    Warmup {
        argc: usize,
        target: DirectCallTarget,
        hits: u8,
    },
    Specialized {
        argc: usize,
        target: DirectCallTarget,
        hits: u64,
        misses: u64,
    },
}

#[derive(Debug, Clone)]
pub(crate) enum DirectCallTarget {
    Closure(Arc<crate::value::VmClosure>),
}

impl PartialEq for DirectCallTarget {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Closure(left), Self::Closure(right)) => Arc::ptr_eq(left, right),
        }
    }
}

impl Eq for DirectCallTarget {}

impl PartialEq for DirectCallState {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Warmup {
                    argc: left_argc,
                    target: left_target,
                    hits: left_hits,
                },
                Self::Warmup {
                    argc: right_argc,
                    target: right_target,
                    hits: right_hits,
                },
            ) => left_argc == right_argc && left_target == right_target && left_hits == right_hits,
            (
                Self::Specialized {
                    argc: left_argc,
                    target: left_target,
                    hits: left_hits,
                    misses: left_misses,
                },
                Self::Specialized {
                    argc: right_argc,
                    target: right_target,
                    hits: right_hits,
                    misses: right_misses,
                },
            ) => {
                left_argc == right_argc
                    && left_target == right_target
                    && left_hits == right_hits
                    && left_misses == right_misses
            }
            _ => false,
        }
    }
}

impl Eq for DirectCallState {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PropertyCacheTarget {
    DictField(Arc<str>),
    StructField { field_name: Arc<str>, index: usize },
    HarnessSubHandle(HarnessKind),
    ListCount,
    ListEmpty,
    ListFirst,
    ListLast,
    StringCount,
    StringEmpty,
    PairFirst,
    PairSecond,
    EnumVariant,
    EnumFields,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MethodCacheTarget {
    Harness(HarnessKind),
    ListCount,
    ListEmpty,
    ListContains,
    StringCount,
    StringEmpty,
    StringContains,
    DictCount,
    DictHas,
    RangeCount,
    RangeLen,
    RangeEmpty,
    RangeFirst,
    RangeLast,
    SetCount,
    SetLen,
    SetEmpty,
    SetContains,
}
