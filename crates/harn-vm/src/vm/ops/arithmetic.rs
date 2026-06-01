use std::sync::Arc;

use crate::chunk::{AdaptiveBinaryOp, AdaptiveBinaryState, BinaryShape, InlineCacheEntry};
use crate::value::{compare_values, values_equal, VmError, VmValue};

const ADAPTIVE_QUICKEN_THRESHOLD: u8 = 3;

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
            VmValue::Int(n) => VmValue::Int(n.wrapping_neg()),
            VmValue::Float(n) => VmValue::Float(-n),
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
        // VM-local cache by scalar chunk metadata. Avoiding a shared Chunk
        // clone here keeps parallel workers from contending on the same
        // compiled closure refcount in arithmetic-heavy pool tasks.
        let (cache_id, slot_count, cache_slot) = {
            let frame = self.frames.last().unwrap();
            let op_offset = frame.ip.saturating_sub(1);
            let cache_id = frame.chunk.cache_id();
            let slot_count = frame.chunk.inline_cache_slot_count();
            let cache_slot = frame.chunk.inline_cache_slot(op_offset);
            (cache_id, slot_count, cache_slot)
        };
        let cached_state = cache_slot
            .and_then(|slot| self.peek_adaptive_binary_cache_by_key(cache_id, slot_count, slot))
            .filter(|(cached_op, _)| *cached_op == op)
            .map(|(_, state)| state);

        let b = self.pop()?;
        let a = self.pop()?;
        let shape = BinaryShape::for_values(op, &a, &b);

        let result = if let Some((result, next_state)) =
            Self::try_specialized_binary(op, cached_state, &a, &b)
        {
            if let Some(slot) = cache_slot {
                self.set_inline_cache_entry_by_key(
                    cache_id,
                    slot_count,
                    slot,
                    InlineCacheEntry::AdaptiveBinary {
                        op,
                        state: next_state,
                    },
                );
            }
            result
        } else {
            let result = Self::generic_binary_result(self, op, a, b)?;
            if let (Some(slot), Some(shape)) = (cache_slot, shape) {
                let next_state = Self::next_adaptive_binary_state(cached_state, shape);
                self.set_inline_cache_entry_by_key(
                    cache_id,
                    slot_count,
                    slot,
                    InlineCacheEntry::AdaptiveBinary {
                        op,
                        state: next_state,
                    },
                );
            }
            result
        };

        self.stack.push(result);
        Ok(())
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

    fn generic_binary_result(
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
            AdaptiveBinaryOp::Less => Ok(VmValue::Bool(compare_values(&a, &b) < 0)),
            AdaptiveBinaryOp::Greater => Ok(VmValue::Bool(compare_values(&a, &b) > 0)),
            AdaptiveBinaryOp::LessEqual => Ok(VmValue::Bool(compare_values(&a, &b) <= 0)),
            AdaptiveBinaryOp::GreaterEqual => Ok(VmValue::Bool(compare_values(&a, &b) >= 0)),
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
                Some(VmValue::Int(x.wrapping_add(*y)))
            }
            (AdaptiveBinaryOp::Sub, BinaryShape::Int, VmValue::Int(x), VmValue::Int(y)) => {
                Some(VmValue::Int(x.wrapping_sub(*y)))
            }
            (AdaptiveBinaryOp::Mul, BinaryShape::Int, VmValue::Int(x), VmValue::Int(y)) => {
                Some(VmValue::Int(x.wrapping_mul(*y)))
            }
            (AdaptiveBinaryOp::Div, BinaryShape::Int, VmValue::Int(_), VmValue::Int(0))
            | (AdaptiveBinaryOp::Mod, BinaryShape::Int, VmValue::Int(_), VmValue::Int(0)) => None,
            (AdaptiveBinaryOp::Div, BinaryShape::Int, VmValue::Int(x), VmValue::Int(y)) => {
                Some(VmValue::Int(x.wrapping_div(*y)))
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
                let ordering = if x < y { -1 } else { i8::from(x > y) };
                Self::specialized_ordering_result(op, ordering, x == y)
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

    fn specialized_equality_result(op: AdaptiveBinaryOp, equal: bool) -> Option<VmValue> {
        let result = match op {
            AdaptiveBinaryOp::Equal => equal,
            AdaptiveBinaryOp::NotEqual => !equal,
            _ => return None,
        };
        Some(VmValue::Bool(result))
    }

    pub(super) fn execute_add_int(&mut self) -> Result<(), VmError> {
        let b = self.pop()?;
        let a = self.pop()?;
        let (x, y) = typed_int_pair("add", a, b)?;
        self.stack.push(VmValue::Int(x.wrapping_add(y)));
        Ok(())
    }

    pub(super) fn execute_sub_int(&mut self) -> Result<(), VmError> {
        let b = self.pop()?;
        let a = self.pop()?;
        let (x, y) = typed_int_pair("subtract", a, b)?;
        self.stack.push(VmValue::Int(x.wrapping_sub(y)));
        Ok(())
    }

    pub(super) fn execute_mul_int(&mut self) -> Result<(), VmError> {
        let b = self.pop()?;
        let a = self.pop()?;
        let (x, y) = typed_int_pair("multiply", a, b)?;
        self.stack.push(VmValue::Int(x.wrapping_mul(y)));
        Ok(())
    }

    pub(super) fn execute_div_int(&mut self) -> Result<(), VmError> {
        let b = self.pop()?;
        let a = self.pop()?;
        let (x, y) = typed_int_pair("divide", a, b)?;
        if y == 0 {
            return Err(VmError::DivisionByZero);
        }
        self.stack.push(VmValue::Int(x.wrapping_div(y)));
        Ok(())
    }

    pub(super) fn execute_mod_int(&mut self) -> Result<(), VmError> {
        let b = self.pop()?;
        let a = self.pop()?;
        let (x, y) = typed_int_pair("modulo", a, b)?;
        if y == 0 {
            return Err(VmError::DivisionByZero);
        }
        self.stack.push(VmValue::Int(x.wrapping_rem(y)));
        Ok(())
    }

    pub(super) fn execute_add_float(&mut self) -> Result<(), VmError> {
        let b = self.pop()?;
        let a = self.pop()?;
        let (x, y) = typed_float_pair("add", a, b)?;
        self.stack.push(VmValue::Float(x + y));
        Ok(())
    }

    pub(super) fn execute_sub_float(&mut self) -> Result<(), VmError> {
        let b = self.pop()?;
        let a = self.pop()?;
        let (x, y) = typed_float_pair("subtract", a, b)?;
        self.stack.push(VmValue::Float(x - y));
        Ok(())
    }

    pub(super) fn execute_mul_float(&mut self) -> Result<(), VmError> {
        let b = self.pop()?;
        let a = self.pop()?;
        let (x, y) = typed_float_pair("multiply", a, b)?;
        self.stack.push(VmValue::Float(x * y));
        Ok(())
    }

    pub(super) fn execute_div_float(&mut self) -> Result<(), VmError> {
        let b = self.pop()?;
        let a = self.pop()?;
        let (x, y) = typed_float_pair("divide", a, b)?;
        self.stack.push(VmValue::Float(x / y));
        Ok(())
    }

    pub(super) fn execute_mod_float(&mut self) -> Result<(), VmError> {
        let b = self.pop()?;
        let a = self.pop()?;
        let (x, y) = typed_float_pair("modulo", a, b)?;
        if y == 0.0 {
            return Err(VmError::DivisionByZero);
        }
        self.stack.push(VmValue::Float(x % y));
        Ok(())
    }

    fn add(&self, a: VmValue, b: VmValue) -> Result<VmValue, VmError> {
        match (a, b) {
            (VmValue::Int(x), VmValue::Int(y)) => Ok(VmValue::Int(x.wrapping_add(y))),
            (VmValue::Float(x), VmValue::Float(y)) => Ok(VmValue::Float(x + y)),
            (VmValue::Int(x), VmValue::Float(y)) => Ok(VmValue::Float(x as f64 + y)),
            (VmValue::Float(x), VmValue::Int(y)) => Ok(VmValue::Float(x + y as f64)),
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
                Ok(VmValue::String(std::sync::Arc::from(s)))
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
                Ok(VmValue::Dict(std::sync::Arc::new(result)))
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
            (VmValue::Int(x), VmValue::Int(y)) => Ok(VmValue::Int(x.wrapping_sub(*y))),
            (VmValue::Float(x), VmValue::Float(y)) => Ok(VmValue::Float(x - y)),
            (VmValue::Int(x), VmValue::Float(y)) => Ok(VmValue::Float(*x as f64 - y)),
            (VmValue::Float(x), VmValue::Int(y)) => Ok(VmValue::Float(x - *y as f64)),
            _ => Err(VmError::TypeError(format!(
                "Cannot subtract {} from {}",
                b.type_name(),
                a.type_name()
            ))),
        }
    }

    fn mul(&self, a: VmValue, b: VmValue) -> Result<VmValue, VmError> {
        match (&a, &b) {
            (VmValue::Int(x), VmValue::Int(y)) => Ok(VmValue::Int(x.wrapping_mul(*y))),
            (VmValue::Float(x), VmValue::Float(y)) => Ok(VmValue::Float(x * y)),
            (VmValue::Int(x), VmValue::Float(y)) => Ok(VmValue::Float(*x as f64 * y)),
            (VmValue::Float(x), VmValue::Int(y)) => Ok(VmValue::Float(x * *y as f64)),
            (VmValue::String(s), VmValue::Int(n)) | (VmValue::Int(n), VmValue::String(s)) => {
                let count = (*n).max(0) as usize;
                Ok(VmValue::String(s.repeat(count).into()))
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
            (VmValue::Int(x), VmValue::Int(y)) => Ok(VmValue::Int(x.wrapping_div(*y))),
            (VmValue::Float(x), VmValue::Float(y)) => Ok(VmValue::Float(x / y)),
            (VmValue::Int(x), VmValue::Float(y)) => Ok(VmValue::Float(*x as f64 / y)),
            (VmValue::Float(x), VmValue::Int(y)) => Ok(VmValue::Float(x / *y as f64)),
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
                    Ok(VmValue::Int(base.wrapping_pow(*exp as u32)))
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

#[inline]
fn typed_int_pair(name: &str, a: VmValue, b: VmValue) -> Result<(i64, i64), VmError> {
    match (a, b) {
        (VmValue::Int(x), VmValue::Int(y)) => Ok((x, y)),
        (a, b) => Err(VmError::TypeError(format!(
            "Typed int {name} expected int operands, got {} and {}",
            a.type_name(),
            b.type_name()
        ))),
    }
}

#[inline]
fn typed_float_pair(name: &str, a: VmValue, b: VmValue) -> Result<(f64, f64), VmError> {
    match (a, b) {
        (VmValue::Float(x), VmValue::Float(y)) => Ok((x, y)),
        (a, b) => Err(VmError::TypeError(format!(
            "Typed float {name} expected float operands, got {} and {}",
            a.type_name(),
            b.type_name()
        ))),
    }
}
