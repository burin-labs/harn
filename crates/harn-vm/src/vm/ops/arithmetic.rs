use std::sync::Arc;

use crate::chunk::{AdaptiveBinaryOp, AdaptiveBinaryState, BinaryShape, InlineCacheEntry};
use crate::value::{try_compare_values, values_equal, VmError, VmValue};

const ADAPTIVE_QUICKEN_THRESHOLD: u8 = 3;

/// Wrap a checked `rust_decimal` arithmetic result: `Some` → a `Decimal`
/// value, `None` → a runtime error (the 96-bit mantissa overflowed) rather
/// than a panic, mirroring how the integer path wraps instead of aborting.
fn decimal_result(value: Option<rust_decimal::Decimal>, op: &str) -> Result<VmValue, VmError> {
    value
        .map(VmValue::decimal)
        .ok_or_else(|| VmError::Runtime(format!("decimal {op} overflowed")))
}

// Scalar `i64` arithmetic that promotes to `f64` on overflow instead of
// silently wrapping two's-complement. This matches the language's own
// aggregate policy — `sum`/`abs` already promote on `i64` overflow (see
// `vm/methods/list.rs` and `vm/methods/iter.rs`) — so a bare `a + b` no
// longer disagrees with `[a, b].sum()`. A wrap produces a wrong magnitude
// silently; promotion preserves it (with float precision), the way Python's
// int→float-on-need and JS's single number tower behave. Decimal keeps its
// own checked-overflow-to-error path (exact money math must not go lossy).
fn int_add(x: i64, y: i64) -> VmValue {
    x.checked_add(y)
        .map_or_else(|| VmValue::Float(x as f64 + y as f64), VmValue::Int)
}

fn int_sub(x: i64, y: i64) -> VmValue {
    x.checked_sub(y)
        .map_or_else(|| VmValue::Float(x as f64 - y as f64), VmValue::Int)
}

fn int_mul(x: i64, y: i64) -> VmValue {
    x.checked_mul(y)
        .map_or_else(|| VmValue::Float(x as f64 * y as f64), VmValue::Int)
}

fn int_div(x: i64, y: i64) -> VmValue {
    // Only `i64::MIN / -1` overflows division (true value `i64::MAX + 1`);
    // promote it rather than wrapping back to `i64::MIN` (a wrong sign and
    // magnitude). Callers guard `y == 0` before reaching here, so `checked_div`
    // returns `None` solely for that overflow. Mirrors `int_neg`/`int_mul`.
    x.checked_div(y)
        .map_or_else(|| VmValue::Float(x as f64 / y as f64), VmValue::Int)
}

fn int_neg(n: i64) -> VmValue {
    // Only `i64::MIN` overflows negation; promote it rather than wrapping
    // back to `i64::MIN` (the classic surprise). Mirrors `abs(i64::MIN)`.
    n.checked_neg()
        .map_or_else(|| VmValue::Float(-(n as f64)), VmValue::Int)
}

fn int_pow(base: i64, exp: u32) -> VmValue {
    base.checked_pow(exp).map_or_else(
        || VmValue::Float((base as f64).powf(exp as f64)),
        VmValue::Int,
    )
}

impl super::super::Vm {
    fn push_binary_result(
        &mut self,
        f: impl FnOnce(&Self, VmValue, VmValue) -> Result<VmValue, VmError>,
    ) -> Result<(), VmError> {
        let b = self.pop()?;
        let a = self.pop()?;
        let result = f(self, a, b)?;
        self.stack.push(result);
        Ok(())
    }

    pub(super) fn execute_add(&mut self) -> Result<(), VmError> {
        self.execute_adaptive_binary(AdaptiveBinaryOp::Add)
    }

    pub(super) fn execute_sub(&mut self) -> Result<(), VmError> {
        self.execute_adaptive_binary(AdaptiveBinaryOp::Sub)
    }

    pub(super) fn execute_mul(&mut self) -> Result<(), VmError> {
        self.execute_adaptive_binary(AdaptiveBinaryOp::Mul)
    }

    pub(super) fn execute_div(&mut self) -> Result<(), VmError> {
        self.execute_adaptive_binary(AdaptiveBinaryOp::Div)
    }

    pub(super) fn execute_mod(&mut self) -> Result<(), VmError> {
        self.execute_adaptive_binary(AdaptiveBinaryOp::Mod)
    }

    pub(super) fn execute_pow(&mut self) -> Result<(), VmError> {
        self.push_binary_result(Self::pow)
    }

    pub(super) fn execute_negate(&mut self) -> Result<(), VmError> {
        let v = self.pop()?;
        self.stack.push(match v {
            VmValue::Int(n) => int_neg(n),
            VmValue::Float(n) => VmValue::Float(-n),
            VmValue::Decimal(d) => VmValue::decimal(-*d),
            _ => {
                return Err(VmError::Runtime(format!(
                    "Cannot negate value of type {}",
                    v.type_name()
                )))
            }
        });
        Ok(())
    }

    pub(super) fn execute_adaptive_binary(&mut self, op: AdaptiveBinaryOp) -> Result<(), VmError> {
        // Read the cache slot from the chunk side table, then access the
        // VM-local cache through the frame-local cache-set index.
        let cache_site = {
            let frame = self.frames.last().unwrap();
            frame.inline_cache_site_for_previous_op()
        };
        let b = self.pop()?;
        let a = self.pop()?;
        let result = self.adaptive_binary_compute(
            op,
            a,
            b,
            cache_site.cache_set,
            cache_site.slot_count,
            cache_site.slot,
        )?;
        self.stack.push(result);
        Ok(())
    }

    /// Shared adaptive-binary core: apply `op` to owned `a`/`b`, consulting and
    /// updating the inline cache slot. Split out of [`Vm::execute_adaptive_binary`]
    /// so other opcodes that synthesize a binary op on operands they already
    /// hold — notably `ConcatAssignLocal` for the `x = x + e` / `x += e`
    /// accumulator — keep the same specialization (e.g. `Int`/`Float` add
    /// quickening) instead of falling back to the generic match on every call.
    pub(super) fn adaptive_binary_compute(
        &mut self,
        op: AdaptiveBinaryOp,
        a: VmValue,
        b: VmValue,
        cache_set: usize,
        slot_count: usize,
        cache_slot: Option<usize>,
    ) -> Result<VmValue, VmError> {
        let cached_state = cache_slot
            .and_then(|slot| self.peek_adaptive_binary_cache_by_index(cache_set, slot))
            .filter(|(cached_op, _)| *cached_op == op)
            .map(|(_, state)| state);

        let shape = BinaryShape::for_values(op, &a, &b);

        if let Some((result, next_state)) = Self::try_specialized_binary(op, cached_state, &a, &b) {
            if let Some(slot) = cache_slot {
                self.set_inline_cache_entry_by_index(
                    cache_set,
                    slot_count,
                    slot,
                    InlineCacheEntry::AdaptiveBinary {
                        op,
                        state: next_state,
                    },
                );
            }
            Ok(result)
        } else {
            let result = Self::generic_binary_result(self, op, a, b)?;
            if let (Some(slot), Some(shape)) = (cache_slot, shape) {
                let next_state = Self::next_adaptive_binary_state(cached_state, shape);
                self.set_inline_cache_entry_by_index(
                    cache_set,
                    slot_count,
                    slot,
                    InlineCacheEntry::AdaptiveBinary {
                        op,
                        state: next_state,
                    },
                );
            }
            Ok(result)
        }
    }

    /// Adaptive-binary specialization fast path. The caller supplies the
    /// peeked `Copy` state after filtering for the current op, which keeps
    /// this helper focused on shape matching and result production. Returns
    /// `(result, next_state)` on a hit; the caller wraps `next_state` into an
    /// `InlineCacheEntry` before writing it back.
    fn try_specialized_binary(
        op: AdaptiveBinaryOp,
        cached_state: Option<AdaptiveBinaryState>,
        a: &VmValue,
        b: &VmValue,
    ) -> Option<(VmValue, AdaptiveBinaryState)> {
        let AdaptiveBinaryState::Specialized {
            shape,
            hits,
            misses,
        } = cached_state?
        else {
            return None;
        };
        if Some(shape) != BinaryShape::for_values(op, a, b) {
            return None;
        }
        let result = Self::specialized_binary_result(op, shape, a, b)?;
        Some((
            result,
            AdaptiveBinaryState::Specialized {
                shape,
                hits: hits.saturating_add(1),
                misses,
            },
        ))
    }

    /// Compute the next adaptive-binary cache state from the peeked
    /// previous state and the freshly-observed `shape`. Operates on
    /// `Copy` state directly — the helper no longer needs to take the
    /// wrapping `InlineCacheEntry` by value (which used to force the
    /// caller to clone the enum on the miss path too).
    fn next_adaptive_binary_state(
        previous: Option<AdaptiveBinaryState>,
        shape: BinaryShape,
    ) -> AdaptiveBinaryState {
        match previous {
            Some(AdaptiveBinaryState::Warmup {
                shape: cached,
                hits,
            }) if cached == shape => {
                let hits = hits.saturating_add(1);
                if hits >= ADAPTIVE_QUICKEN_THRESHOLD {
                    AdaptiveBinaryState::Specialized {
                        shape,
                        hits: hits as u64,
                        misses: 0,
                    }
                } else {
                    AdaptiveBinaryState::Warmup { shape, hits }
                }
            }
            Some(AdaptiveBinaryState::Specialized {
                shape: cached,
                hits,
                misses,
            }) if cached == shape => AdaptiveBinaryState::Specialized {
                shape,
                hits: hits.saturating_add(1),
                misses,
            },
            Some(AdaptiveBinaryState::Specialized { misses: 0, .. }) => {
                AdaptiveBinaryState::Specialized {
                    shape,
                    hits: 1,
                    misses: 1,
                }
            }
            _ => AdaptiveBinaryState::Warmup { shape, hits: 1 },
        }
    }

    /// Generic (non-specialized) result for a binary op, matching exactly what
    /// the unoptimized build computes. Exposed so the typed fast-path opcodes can
    /// fall back to it on an operand-type miss — see [`Self::run_typed_binary`].
    pub(super) fn generic_binary_result(
        vm: &Self,
        op: AdaptiveBinaryOp,
        a: VmValue,
        b: VmValue,
    ) -> Result<VmValue, VmError> {
        match op {
            AdaptiveBinaryOp::Add => vm.add(a, b),
            AdaptiveBinaryOp::Sub => vm.sub(a, b),
            AdaptiveBinaryOp::Mul => vm.mul(a, b),
            AdaptiveBinaryOp::Div => vm.div(a, b),
            AdaptiveBinaryOp::Mod => vm.modulo(a, b),
            AdaptiveBinaryOp::Equal => Ok(VmValue::Bool(values_equal(&a, &b))),
            AdaptiveBinaryOp::NotEqual => Ok(VmValue::Bool(!values_equal(&a, &b))),
            // NaN is unordered: `try_compare_values` returns `None`, so every
            // relational operator yields `false` (per IEEE-754 / the spec).
            AdaptiveBinaryOp::Less => Ok(VmValue::Bool(
                matches!(try_compare_values(&a, &b), Some(o) if o < 0),
            )),
            AdaptiveBinaryOp::Greater => Ok(VmValue::Bool(
                matches!(try_compare_values(&a, &b), Some(o) if o > 0),
            )),
            AdaptiveBinaryOp::LessEqual => Ok(VmValue::Bool(
                matches!(try_compare_values(&a, &b), Some(o) if o <= 0),
            )),
            AdaptiveBinaryOp::GreaterEqual => Ok(VmValue::Bool(
                matches!(try_compare_values(&a, &b), Some(o) if o >= 0),
            )),
        }
    }

    fn specialized_binary_result(
        op: AdaptiveBinaryOp,
        shape: BinaryShape,
        a: &VmValue,
        b: &VmValue,
    ) -> Option<VmValue> {
        match (op, shape, a, b) {
            (AdaptiveBinaryOp::Add, BinaryShape::Int, VmValue::Int(x), VmValue::Int(y)) => {
                Some(int_add(*x, *y))
            }
            (AdaptiveBinaryOp::Sub, BinaryShape::Int, VmValue::Int(x), VmValue::Int(y)) => {
                Some(int_sub(*x, *y))
            }
            (AdaptiveBinaryOp::Mul, BinaryShape::Int, VmValue::Int(x), VmValue::Int(y)) => {
                Some(int_mul(*x, *y))
            }
            (AdaptiveBinaryOp::Div, BinaryShape::Int, VmValue::Int(_), VmValue::Int(0))
            | (AdaptiveBinaryOp::Mod, BinaryShape::Int, VmValue::Int(_), VmValue::Int(0)) => None,
            (AdaptiveBinaryOp::Div, BinaryShape::Int, VmValue::Int(x), VmValue::Int(y)) => {
                Some(int_div(*x, *y))
            }
            (AdaptiveBinaryOp::Mod, BinaryShape::Int, VmValue::Int(x), VmValue::Int(y)) => {
                Some(VmValue::Int(x.wrapping_rem(*y)))
            }
            (AdaptiveBinaryOp::Add, BinaryShape::Float, VmValue::Float(x), VmValue::Float(y)) => {
                Some(VmValue::Float(x + y))
            }
            (AdaptiveBinaryOp::Sub, BinaryShape::Float, VmValue::Float(x), VmValue::Float(y)) => {
                Some(VmValue::Float(x - y))
            }
            (AdaptiveBinaryOp::Mul, BinaryShape::Float, VmValue::Float(x), VmValue::Float(y)) => {
                Some(VmValue::Float(x * y))
            }
            (AdaptiveBinaryOp::Div, BinaryShape::Float, VmValue::Float(x), VmValue::Float(y)) => {
                Some(VmValue::Float(x / y))
            }
            (AdaptiveBinaryOp::Mod, BinaryShape::Float, VmValue::Float(_), VmValue::Float(0.0)) => {
                None
            }
            (AdaptiveBinaryOp::Mod, BinaryShape::Float, VmValue::Float(x), VmValue::Float(y)) => {
                Some(VmValue::Float(x % y))
            }
            (_, BinaryShape::Int, VmValue::Int(x), VmValue::Int(y)) => {
                let ordering = match x.cmp(y) {
                    std::cmp::Ordering::Less => -1,
                    std::cmp::Ordering::Equal => 0,
                    std::cmp::Ordering::Greater => 1,
                };
                Self::specialized_ordering_result(op, ordering, x == y)
            }
            (_, BinaryShape::Float, VmValue::Float(x), VmValue::Float(y)) => {
                // NaN is unordered: `partial_cmp` is `None`, so relational
                // operators must all yield `false` (matching IEEE-754).
                match x.partial_cmp(y) {
                    Some(ord) => Self::specialized_ordering_result(op, ord as i8, x == y),
                    None => Self::specialized_unordered_result(op),
                }
            }
            (_, BinaryShape::Bool, VmValue::Bool(x), VmValue::Bool(y)) => {
                Self::specialized_equality_result(op, x == y)
            }
            (_, BinaryShape::String, VmValue::String(x), VmValue::String(y)) => {
                Self::specialized_equality_result(op, x == y)
            }
            _ => None,
        }
    }

    fn specialized_ordering_result(
        op: AdaptiveBinaryOp,
        ordering: i8,
        equal: bool,
    ) -> Option<VmValue> {
        let result = match op {
            AdaptiveBinaryOp::Equal => equal,
            AdaptiveBinaryOp::NotEqual => !equal,
            AdaptiveBinaryOp::Less => ordering < 0,
            AdaptiveBinaryOp::Greater => ordering > 0,
            AdaptiveBinaryOp::LessEqual => ordering <= 0,
            AdaptiveBinaryOp::GreaterEqual => ordering >= 0,
            _ => return None,
        };
        Some(VmValue::Bool(result))
    }

    /// Result for an *unordered* comparison (a NaN operand): `==` is false,
    /// `!=` is true, and every relational operator (`<`, `>`, `<=`, `>=`) is
    /// false, matching IEEE-754 semantics.
    fn specialized_unordered_result(op: AdaptiveBinaryOp) -> Option<VmValue> {
        let result = match op {
            AdaptiveBinaryOp::Equal => false,
            AdaptiveBinaryOp::NotEqual => true,
            AdaptiveBinaryOp::Less
            | AdaptiveBinaryOp::Greater
            | AdaptiveBinaryOp::LessEqual
            | AdaptiveBinaryOp::GreaterEqual => false,
            _ => return None,
        };
        Some(VmValue::Bool(result))
    }

    fn specialized_equality_result(op: AdaptiveBinaryOp, equal: bool) -> Option<VmValue> {
        let result = match op {
            AdaptiveBinaryOp::Equal => equal,
            AdaptiveBinaryOp::NotEqual => !equal,
            _ => return None,
        };
        Some(VmValue::Bool(result))
    }

    /// Shared driver for the typed fast-path binary opcodes. Pops the two
    /// operands, runs the supplied monomorphic fast path, and — when the
    /// operands do not match the specialized shape — falls back to the exact
    /// generic result the unoptimized build would produce (`op`).
    ///
    /// This is the runtime-guard half of typed-opcode specialization. The
    /// compiler emits a typed op (`AddInt`, `LessInt`, …) from a *static* type
    /// guess, but a guess can be wrong at runtime — e.g. an `any`-typed value
    /// flowing through a typed parameter or an annotated binding initializer is
    /// not runtime-checked, so the operand may be a different primitive than the
    /// annotation claims. Hard-erroring there made the optimized build throw on
    /// programs the unoptimized build runs correctly; guarding and falling back
    /// keeps `optimized ≡ unoptimized` by construction. The fast path is a
    /// monomorphic match the optimizer fully inlines, so the common case where
    /// the guess holds pays nothing beyond the type check it already performed.
    #[inline]
    pub(super) fn run_typed_binary(
        &mut self,
        op: AdaptiveBinaryOp,
        fast: impl FnOnce(&VmValue, &VmValue) -> Option<Result<VmValue, VmError>>,
    ) -> Result<(), VmError> {
        let b = self.pop()?;
        let a = self.pop()?;
        let result = match fast(&a, &b) {
            Some(result) => result,
            None => Self::generic_binary_result(self, op, a, b),
        }?;
        self.stack.push(result);
        Ok(())
    }

    pub(super) fn execute_add_int(&mut self) -> Result<(), VmError> {
        self.run_typed_binary(AdaptiveBinaryOp::Add, |a, b| match (a, b) {
            (VmValue::Int(x), VmValue::Int(y)) => Some(Ok(int_add(*x, *y))),
            _ => None,
        })
    }

    pub(super) fn execute_sub_int(&mut self) -> Result<(), VmError> {
        self.run_typed_binary(AdaptiveBinaryOp::Sub, |a, b| match (a, b) {
            (VmValue::Int(x), VmValue::Int(y)) => Some(Ok(int_sub(*x, *y))),
            _ => None,
        })
    }

    pub(super) fn execute_mul_int(&mut self) -> Result<(), VmError> {
        self.run_typed_binary(AdaptiveBinaryOp::Mul, |a, b| match (a, b) {
            (VmValue::Int(x), VmValue::Int(y)) => Some(Ok(int_mul(*x, *y))),
            _ => None,
        })
    }

    pub(super) fn execute_div_int(&mut self) -> Result<(), VmError> {
        self.run_typed_binary(AdaptiveBinaryOp::Div, |a, b| match (a, b) {
            (VmValue::Int(_), VmValue::Int(0)) => Some(Err(VmError::DivisionByZero)),
            (VmValue::Int(x), VmValue::Int(y)) => Some(Ok(int_div(*x, *y))),
            _ => None,
        })
    }

    pub(super) fn execute_mod_int(&mut self) -> Result<(), VmError> {
        self.run_typed_binary(AdaptiveBinaryOp::Mod, |a, b| match (a, b) {
            (VmValue::Int(_), VmValue::Int(0)) => Some(Err(VmError::DivisionByZero)),
            (VmValue::Int(x), VmValue::Int(y)) => Some(Ok(VmValue::Int(x.wrapping_rem(*y)))),
            _ => None,
        })
    }

    pub(super) fn execute_add_float(&mut self) -> Result<(), VmError> {
        self.run_typed_binary(AdaptiveBinaryOp::Add, |a, b| match (a, b) {
            (VmValue::Float(x), VmValue::Float(y)) => Some(Ok(VmValue::Float(x + y))),
            _ => None,
        })
    }

    pub(super) fn execute_sub_float(&mut self) -> Result<(), VmError> {
        self.run_typed_binary(AdaptiveBinaryOp::Sub, |a, b| match (a, b) {
            (VmValue::Float(x), VmValue::Float(y)) => Some(Ok(VmValue::Float(x - y))),
            _ => None,
        })
    }

    pub(super) fn execute_mul_float(&mut self) -> Result<(), VmError> {
        self.run_typed_binary(AdaptiveBinaryOp::Mul, |a, b| match (a, b) {
            (VmValue::Float(x), VmValue::Float(y)) => Some(Ok(VmValue::Float(x * y))),
            _ => None,
        })
    }

    pub(super) fn execute_div_float(&mut self) -> Result<(), VmError> {
        self.run_typed_binary(AdaptiveBinaryOp::Div, |a, b| match (a, b) {
            (VmValue::Float(x), VmValue::Float(y)) => Some(Ok(VmValue::Float(x / y))),
            _ => None,
        })
    }

    pub(super) fn execute_mod_float(&mut self) -> Result<(), VmError> {
        self.run_typed_binary(AdaptiveBinaryOp::Mod, |a, b| match (a, b) {
            (VmValue::Float(_), VmValue::Float(y)) if *y == 0.0 => {
                Some(Err(VmError::DivisionByZero))
            }
            (VmValue::Float(x), VmValue::Float(y)) => Some(Ok(VmValue::Float(x % y))),
            _ => None,
        })
    }

    fn add(&self, a: VmValue, b: VmValue) -> Result<VmValue, VmError> {
        match (a, b) {
            (VmValue::Int(x), VmValue::Int(y)) => Ok(int_add(x, y)),
            (VmValue::Float(x), VmValue::Float(y)) => Ok(VmValue::Float(x + y)),
            (VmValue::Int(x), VmValue::Float(y)) => Ok(VmValue::Float(x as f64 + y)),
            (VmValue::Float(x), VmValue::Int(y)) => Ok(VmValue::Float(x + y as f64)),
            // Decimal arithmetic: Decimal⊕Decimal and Decimal⊕Int (Int promoted
            // exactly). Decimal⊕Float is intentionally absent — it falls to the
            // type-error arm below so lossy binary floats never silently enter
            // exact money math. `checked_*` returns an error (not a panic) on
            // the rare overflow of a 96-bit mantissa.
            (VmValue::Decimal(x), VmValue::Decimal(y)) => {
                decimal_result(x.checked_add(*y), "addition")
            }
            (VmValue::Decimal(x), VmValue::Int(y)) => {
                decimal_result(x.checked_add(rust_decimal::Decimal::from(y)), "addition")
            }
            (VmValue::Int(x), VmValue::Decimal(y)) => {
                decimal_result(rust_decimal::Decimal::from(x).checked_add(*y), "addition")
            }
            (VmValue::String(x), VmValue::String(y)) => {
                if x.is_empty() {
                    return Ok(VmValue::String(y));
                }
                if y.is_empty() {
                    return Ok(VmValue::String(x));
                }
                let mut s = String::with_capacity(x.len() + y.len());
                s.push_str(&x);
                s.push_str(&y);
                Ok(VmValue::String(arcstr::ArcStr::from(s)))
            }
            (VmValue::List(x), VmValue::List(y)) => {
                if x.is_empty() {
                    return Ok(VmValue::List(y));
                }
                if y.is_empty() {
                    return Ok(VmValue::List(x));
                }
                let y_len = y.len();
                let mut result = Arc::try_unwrap(x).unwrap_or_else(|items| items.as_ref().clone());
                result.reserve(y_len);
                match Arc::try_unwrap(y) {
                    Ok(items) => result.extend(items),
                    Err(items) => result.extend(items.iter().cloned()),
                }
                Ok(VmValue::List(std::sync::Arc::new(result)))
            }
            (VmValue::Dict(x), VmValue::Dict(y)) => {
                if x.is_empty() {
                    return Ok(VmValue::Dict(y));
                }
                if y.is_empty() {
                    return Ok(VmValue::Dict(x));
                }
                let mut result =
                    Arc::try_unwrap(x).unwrap_or_else(|entries| entries.as_ref().clone());
                match Arc::try_unwrap(y) {
                    Ok(entries) => result.extend(entries),
                    Err(entries) => {
                        result.extend(entries.iter().map(|(k, v)| (k.clone(), v.clone())));
                    }
                }
                Ok(VmValue::dict(result))
            }
            (a, b) => Err(VmError::TypeError(format!(
                "Cannot add {} and {}",
                a.type_name(),
                b.type_name()
            ))),
        }
    }

    fn sub(&self, a: VmValue, b: VmValue) -> Result<VmValue, VmError> {
        match (&a, &b) {
            (VmValue::Int(x), VmValue::Int(y)) => Ok(int_sub(*x, *y)),
            (VmValue::Float(x), VmValue::Float(y)) => Ok(VmValue::Float(x - y)),
            (VmValue::Int(x), VmValue::Float(y)) => Ok(VmValue::Float(*x as f64 - y)),
            (VmValue::Float(x), VmValue::Int(y)) => Ok(VmValue::Float(x - *y as f64)),
            (VmValue::Decimal(x), VmValue::Decimal(y)) => {
                decimal_result(x.checked_sub(**y), "subtraction")
            }
            (VmValue::Decimal(x), VmValue::Int(y)) => decimal_result(
                x.checked_sub(rust_decimal::Decimal::from(*y)),
                "subtraction",
            ),
            (VmValue::Int(x), VmValue::Decimal(y)) => decimal_result(
                rust_decimal::Decimal::from(*x).checked_sub(**y),
                "subtraction",
            ),
            _ => Err(VmError::TypeError(format!(
                "Cannot subtract {} from {}",
                b.type_name(),
                a.type_name()
            ))),
        }
    }

    fn mul(&self, a: VmValue, b: VmValue) -> Result<VmValue, VmError> {
        match (&a, &b) {
            (VmValue::Int(x), VmValue::Int(y)) => Ok(int_mul(*x, *y)),
            (VmValue::Float(x), VmValue::Float(y)) => Ok(VmValue::Float(x * y)),
            (VmValue::Int(x), VmValue::Float(y)) => Ok(VmValue::Float(*x as f64 * y)),
            (VmValue::Float(x), VmValue::Int(y)) => Ok(VmValue::Float(x * *y as f64)),
            (VmValue::Decimal(x), VmValue::Decimal(y)) => {
                decimal_result(x.checked_mul(**y), "multiplication")
            }
            (VmValue::Decimal(x), VmValue::Int(y)) => decimal_result(
                x.checked_mul(rust_decimal::Decimal::from(*y)),
                "multiplication",
            ),
            (VmValue::Int(x), VmValue::Decimal(y)) => decimal_result(
                rust_decimal::Decimal::from(*x).checked_mul(**y),
                "multiplication",
            ),
            (VmValue::String(s), VmValue::Int(n)) | (VmValue::Int(n), VmValue::String(s)) => {
                // Guard script-controlled repeat counts so `"a" * 1_000_000_000`
                // errors cleanly instead of OOM-ing / panicking `capacity overflow`.
                let count = (*n).max(0) as usize;
                Ok(VmValue::String(
                    crate::limits::checked_repeat(s, count)?.into(),
                ))
            }
            _ => Err(VmError::TypeError(format!(
                "Cannot multiply {} and {}",
                a.type_name(),
                b.type_name()
            ))),
        }
    }

    fn div(&self, a: VmValue, b: VmValue) -> Result<VmValue, VmError> {
        match (&a, &b) {
            (VmValue::Int(_), VmValue::Int(y)) if *y == 0 => Err(VmError::DivisionByZero),
            (VmValue::Int(x), VmValue::Int(y)) => Ok(int_div(*x, *y)),
            (VmValue::Float(x), VmValue::Float(y)) => Ok(VmValue::Float(x / y)),
            (VmValue::Int(x), VmValue::Float(y)) => Ok(VmValue::Float(*x as f64 / y)),
            (VmValue::Float(x), VmValue::Int(y)) => Ok(VmValue::Float(x / *y as f64)),
            (VmValue::Decimal(_), VmValue::Decimal(y)) if **y == rust_decimal::Decimal::ZERO => {
                Err(VmError::DivisionByZero)
            }
            (VmValue::Decimal(x), VmValue::Decimal(y)) => {
                decimal_result(x.checked_div(**y), "division")
            }
            (VmValue::Decimal(_), VmValue::Int(0)) => Err(VmError::DivisionByZero),
            (VmValue::Decimal(x), VmValue::Int(y)) => {
                decimal_result(x.checked_div(rust_decimal::Decimal::from(*y)), "division")
            }
            (VmValue::Int(_), VmValue::Decimal(y)) if **y == rust_decimal::Decimal::ZERO => {
                Err(VmError::DivisionByZero)
            }
            (VmValue::Int(x), VmValue::Decimal(y)) => {
                decimal_result(rust_decimal::Decimal::from(*x).checked_div(**y), "division")
            }
            _ => Err(VmError::Runtime(format!(
                "Cannot divide {} by {}",
                a.type_name(),
                b.type_name()
            ))),
        }
    }

    fn modulo(&self, a: VmValue, b: VmValue) -> Result<VmValue, VmError> {
        match (&a, &b) {
            (VmValue::Int(_), VmValue::Int(y)) if *y == 0 => Err(VmError::DivisionByZero),
            (VmValue::Int(x), VmValue::Int(y)) => Ok(VmValue::Int(x.wrapping_rem(*y))),
            (VmValue::Float(_), VmValue::Float(y)) if *y == 0.0 => Err(VmError::DivisionByZero),
            (VmValue::Float(x), VmValue::Float(y)) => Ok(VmValue::Float(x % y)),
            (VmValue::Int(_), VmValue::Float(y)) if *y == 0.0 => Err(VmError::DivisionByZero),
            (VmValue::Int(x), VmValue::Float(y)) => Ok(VmValue::Float(*x as f64 % y)),
            (VmValue::Float(_), VmValue::Int(y)) if *y == 0 => Err(VmError::DivisionByZero),
            (VmValue::Float(x), VmValue::Int(y)) => Ok(VmValue::Float(x % *y as f64)),
            (VmValue::Decimal(_), VmValue::Decimal(y)) if **y == rust_decimal::Decimal::ZERO => {
                Err(VmError::DivisionByZero)
            }
            (VmValue::Decimal(x), VmValue::Decimal(y)) => {
                decimal_result(x.checked_rem(**y), "modulo")
            }
            (VmValue::Decimal(_), VmValue::Int(0)) => Err(VmError::DivisionByZero),
            (VmValue::Decimal(x), VmValue::Int(y)) => {
                decimal_result(x.checked_rem(rust_decimal::Decimal::from(*y)), "modulo")
            }
            (VmValue::Int(_), VmValue::Decimal(y)) if **y == rust_decimal::Decimal::ZERO => {
                Err(VmError::DivisionByZero)
            }
            (VmValue::Int(x), VmValue::Decimal(y)) => {
                decimal_result(rust_decimal::Decimal::from(*x).checked_rem(**y), "modulo")
            }
            _ => Err(VmError::Runtime(format!(
                "Cannot modulo {} by {}",
                a.type_name(),
                b.type_name()
            ))),
        }
    }

    fn pow(&self, a: VmValue, b: VmValue) -> Result<VmValue, VmError> {
        match (&a, &b) {
            (VmValue::Int(base), VmValue::Int(exp)) => {
                if u32::try_from(*exp).is_ok() {
                    Ok(int_pow(*base, *exp as u32))
                } else {
                    Ok(VmValue::Float((*base as f64).powf(*exp as f64)))
                }
            }
            (VmValue::Float(base), VmValue::Int(exp)) => {
                if i32::try_from(*exp).is_ok() {
                    Ok(VmValue::Float(base.powi(*exp as i32)))
                } else {
                    Ok(VmValue::Float(base.powf(*exp as f64)))
                }
            }
            (VmValue::Int(base), VmValue::Float(exp)) => {
                Ok(VmValue::Float((*base as f64).powf(*exp)))
            }
            (VmValue::Float(base), VmValue::Float(exp)) => Ok(VmValue::Float(base.powf(*exp))),
            _ => Err(VmError::TypeError(format!(
                "Cannot exponentiate {} by {}",
                a.type_name(),
                b.type_name()
            ))),
        }
    }
}

impl BinaryShape {
    fn for_values(op: AdaptiveBinaryOp, a: &VmValue, b: &VmValue) -> Option<Self> {
        match (a, b) {
            (VmValue::Int(_), VmValue::Int(_)) => Some(Self::Int),
            (VmValue::Float(_), VmValue::Float(_)) => Some(Self::Float),
            (VmValue::Bool(_), VmValue::Bool(_))
                if matches!(op, AdaptiveBinaryOp::Equal | AdaptiveBinaryOp::NotEqual) =>
            {
                Some(Self::Bool)
            }
            (VmValue::String(_), VmValue::String(_))
                if matches!(op, AdaptiveBinaryOp::Equal | AdaptiveBinaryOp::NotEqual) =>
            {
                Some(Self::String)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod overflow_promotion_tests {
    use super::*;

    #[test]
    fn add_sub_mul_promote_to_float_on_overflow() {
        assert!(matches!(int_add(2, 3), VmValue::Int(5)));
        assert!(matches!(int_add(i64::MAX, 1), VmValue::Float(_)));
        assert!(matches!(int_sub(i64::MIN, 1), VmValue::Float(_)));
        assert!(matches!(int_mul(i64::MAX, 2), VmValue::Float(_)));
        // In-range stays int.
        assert!(matches!(int_mul(1000, 1000), VmValue::Int(1_000_000)));
    }

    #[test]
    fn neg_of_i64_min_promotes_instead_of_wrapping() {
        // The classic two's-complement surprise: `-i64::MIN` wraps back to
        // `i64::MIN`. Promotion yields the correct positive magnitude.
        match int_neg(i64::MIN) {
            VmValue::Float(f) => assert!(f > 0.0),
            other => panic!("expected promoted float, got {other:?}"),
        }
        assert!(matches!(int_neg(5), VmValue::Int(-5)));
    }

    #[test]
    fn pow_promotes_on_overflow() {
        assert!(matches!(int_pow(2, 10), VmValue::Int(1024)));
        assert!(matches!(int_pow(2, 100), VmValue::Float(_)));
    }

    #[test]
    fn div_of_i64_min_by_neg_one_promotes_instead_of_wrapping() {
        // `i64::MIN / -1` is the one division that overflows: its true value is
        // `i64::MAX + 1`, but two's-complement wraps it back to `i64::MIN` (a
        // wrong sign and magnitude). Promotion preserves the magnitude, the same
        // way `int_neg(i64::MIN)` and `abs(i64::MIN)` already do.
        match int_div(i64::MIN, -1) {
            VmValue::Float(f) => assert!(f > 0.0),
            other => panic!("expected promoted float, got {other:?}"),
        }
        // In-range division stays int (truncating toward zero, as before).
        assert!(matches!(int_div(7, 2), VmValue::Int(3)));
        assert!(matches!(int_div(i64::MIN, 1), VmValue::Int(i64::MIN)));
    }
}
