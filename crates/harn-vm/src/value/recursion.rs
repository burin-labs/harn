//! Stack-safety guards for recursive walks over arbitrarily deep `VmValue`
//! trees.
//!
//! Harn scripts can build values nested far deeper than the native call stack
//! tolerates without ever tripping a runtime limit: `x = [x]` inside a loop
//! adds no VM call frames, so [`crate::runtime_limits::RuntimeLimits::max_vm_frames`]
//! never fires, and a `200_000`-deep list costs only a few MB. Walking such a
//! value recursively — for `==`, ordering, hashing, display, or JSON
//! serialization — then overflows the OS thread stack and aborts the whole
//! process (`SIGABRT`), an uncatchable failure that bypasses every VM resource
//! guard. Dropping the value has the same hazard.
//!
//! Two complementary mechanisms keep these walks safe:
//!
//! * [`guard_recursion`] grows the native stack on demand (the approach serde,
//!   rustc, and syn take via the `stacker` crate) so a deep-but-finite value
//!   completes instead of crashing. It is applied around the *recursive
//!   descent* in container arms only, so scalar and wide collections pay
//!   nothing while deep nesting grows the stack one level at a time.
//! * [`dismantle_values`] tears values down iteratively, used by `Drop` impls
//!   that own nested values so teardown never recurses on the native stack.
//!   This is sound precisely because `VmValue` deliberately has no `Drop`
//!   impl, so children can be moved out of uniquely-owned `Arc`s.

use std::sync::Arc;

use super::VmValue;

/// Native stack we insist on having free before recursing one more container
/// level. When less remains, [`guard_recursion`] allocates a fresh segment.
const RED_ZONE: usize = 256 * 1024;

/// Size of each on-demand stack segment [`guard_recursion`] allocates.
const STACK_PER_SEGMENT: usize = 4 * 1024 * 1024;

/// Run `f`, first ensuring at least [`RED_ZONE`] bytes of native stack are
/// available and growing onto a fresh segment otherwise. When the stack is
/// ample (the overwhelmingly common case) this is a single stack-pointer
/// comparison, so callers can wrap each recursive descent cheaply.
#[inline]
pub fn guard_recursion<R>(f: impl FnOnce() -> R) -> R {
    stacker::maybe_grow(RED_ZONE, STACK_PER_SEGMENT, f)
}

/// Whether `value` owns nested `VmValue`s and therefore could form an
/// arbitrarily deep chain whose recursive drop would overflow the stack.
#[inline]
pub fn is_recursive_container(value: &VmValue) -> bool {
    matches!(
        value,
        VmValue::List(_)
            | VmValue::Set(_)
            | VmValue::Dict(_)
            | VmValue::Pair(_)
            | VmValue::EnumVariant(_)
            | VmValue::StructInstance { .. }
            | VmValue::Closure(_)
    )
}

/// The greatest nesting depth in `value`, capped at `limit + 1` so the scan
/// stays cheap and itself never recurses. A flat scalar is depth `0`; a list
/// of scalars is depth `1`. Used by serializers that must reject values too
/// deep for a third-party recursive encoder.
pub fn depth_within(value: &VmValue, limit: usize) -> bool {
    // Iterative DFS with an explicit (value, depth) worklist so the depth
    // probe cannot itself overflow.
    let mut stack: Vec<(&VmValue, usize)> = vec![(value, 0)];
    while let Some((value, depth)) = stack.pop() {
        if depth > limit {
            return false;
        }
        match value {
            VmValue::List(items) | VmValue::Set(items) => {
                for item in items.iter() {
                    stack.push((item, depth + 1));
                }
            }
            VmValue::Dict(map) => {
                for item in map.values() {
                    stack.push((item, depth + 1));
                }
            }
            VmValue::Pair(pair) => {
                stack.push((&pair.0, depth + 1));
                stack.push((&pair.1, depth + 1));
            }
            VmValue::EnumVariant(variant) => {
                for field in variant.fields.iter() {
                    stack.push((field, depth + 1));
                }
            }
            VmValue::StructInstance { fields, .. } => {
                for field in fields.iter().flatten() {
                    stack.push((field, depth + 1));
                }
            }
            _ => {}
        }
    }
    true
}

/// Drop a batch of values without recursing on the native stack.
///
/// Nested children of uniquely-owned containers are moved onto an explicit
/// heap worklist and processed iteratively, so a value nested arbitrarily deep
/// tears down in bounded stack space. Shared payloads (`Arc::into_inner`
/// returns `None`) are left intact for their last owner to reclaim — which,
/// for script values, is itself a `Scope`/closure whose `Drop` routes back
/// here.
/// Drop a single value without recursing on the native stack. Cheap for
/// scalars; see [`dismantle_values`] for the iterative teardown of containers.
#[inline]
pub fn dismantle(value: VmValue) {
    if is_recursive_container(&value) {
        dismantle_values(std::iter::once(value));
    }
}

pub fn dismantle_values<I: IntoIterator<Item = VmValue>>(values: I) {
    let mut stack: Vec<VmValue> = values.into_iter().collect();
    while let Some(value) = stack.pop() {
        match value {
            VmValue::List(items) | VmValue::Set(items) => {
                if let Some(mut items) = Arc::into_inner(items) {
                    stack.append(&mut items);
                }
            }
            VmValue::Dict(map) => {
                if let Some(map) = Arc::into_inner(map) {
                    stack.extend(map.into_values());
                }
            }
            VmValue::Pair(pair) => {
                if let Some(pair) = Arc::into_inner(pair) {
                    stack.push(pair.0);
                    stack.push(pair.1);
                }
            }
            VmValue::EnumVariant(variant) => {
                if let Some(variant) = Arc::into_inner(variant) {
                    if let Some(mut fields) = Arc::into_inner(variant.fields) {
                        stack.append(&mut fields);
                    }
                }
            }
            VmValue::StructInstance { fields, .. } => {
                if let Some(fields) = Arc::into_inner(fields) {
                    stack.extend(fields.into_iter().flatten());
                }
            }
            VmValue::Closure(closure) => {
                // A closure can capture nested values in its environment. When
                // we hold the last reference, take ownership of the captured
                // scopes' bindings so they tear down iteratively too; the
                // `Scope` `Drop` impl re-routes here for anything still shared.
                if let Some(mut closure) = Arc::into_inner(closure) {
                    for scope in &mut closure.env.scopes {
                        if let Some(vars) = Arc::get_mut(&mut scope.vars) {
                            stack.extend(std::mem::take(vars).into_values().map(|(v, _)| v));
                        }
                    }
                }
            }
            // Scalars and identity/handle values: dropped here, no recursion.
            _ => {}
        }
    }
}
