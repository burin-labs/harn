//! VM-local memo for receipt-equivalent runtime effect calls.
//!
//! The shared recorder remains authoritative across child VMs. This cache only
//! skips repeat materialization within one executor when the immutable contract
//! and every resource-bearing argument match a recent call in the selected set.

use crate::VmValue;

const RUNTIME_EFFECT_CACHE_SETS: usize = 16;
const RUNTIME_EFFECT_CACHE_WAYS: usize = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct EffectContractKey {
    specs: usize,
    len: usize,
}

impl EffectContractKey {
    fn new(specs: &'static [harn_builtin_meta::EffectSpec]) -> Self {
        Self {
            specs: specs.as_ptr() as usize,
            len: specs.len(),
        }
    }
}

struct CachedRuntimeEffectCall {
    contract: EffectContractKey,
    arguments: Vec<Vec<crate::value::HarnStr>>,
}

/// Direct-mapped memo for receipt-equivalent effect calls.
///
/// Collisions and evictions cost work but cannot drop evidence: a miss falls
/// through to the shared recorder.
pub(crate) struct RuntimeEffectCallCache {
    sets: Vec<[Option<CachedRuntimeEffectCall>; RUNTIME_EFFECT_CACHE_WAYS]>,
    next_replacement: Vec<usize>,
}

impl Default for RuntimeEffectCallCache {
    fn default() -> Self {
        Self {
            sets: (0..RUNTIME_EFFECT_CACHE_SETS)
                .map(|_| std::array::from_fn(|_| None))
                .collect(),
            next_replacement: vec![0; RUNTIME_EFFECT_CACHE_SETS],
        }
    }
}

impl RuntimeEffectCallCache {
    fn set_index(contract: EffectContractKey) -> usize {
        ((contract.specs >> 4) ^ contract.len) & (RUNTIME_EFFECT_CACHE_SETS - 1)
    }

    pub(crate) fn contains(
        &self,
        specs: &'static [harn_builtin_meta::EffectSpec],
        args: &[VmValue],
    ) -> bool {
        let contract = EffectContractKey::new(specs);
        self.sets[Self::set_index(contract)]
            .iter()
            .flatten()
            .any(|cached| {
                cached.contract == contract
                    && runtime_effect_arguments_match(&cached.arguments, specs, args)
            })
    }

    pub(crate) fn remember(
        &mut self,
        specs: &'static [harn_builtin_meta::EffectSpec],
        args: &[VmValue],
    ) {
        let contract = EffectContractKey::new(specs);
        let set_index = Self::set_index(contract);
        let way = self.sets[set_index]
            .iter()
            .position(Option::is_none)
            .unwrap_or(self.next_replacement[set_index]);
        self.sets[set_index][way] = Some(CachedRuntimeEffectCall {
            contract,
            arguments: runtime_effect_arguments(specs, args),
        });
        self.next_replacement[set_index] = (way + 1) % RUNTIME_EFFECT_CACHE_WAYS;
    }

    pub(crate) fn clear(&mut self) {
        for set in &mut self.sets {
            for entry in set {
                *entry = None;
            }
        }
        self.next_replacement.fill(0);
    }
}

pub(super) fn resolve_runtime_resources(
    selector: harn_builtin_meta::ResourceSelector,
    args: &[VmValue],
) -> Vec<crate::value::HarnStr> {
    use harn_builtin_meta::ResourceSelector;
    match selector {
        ResourceSelector::Argument(index) => args
            .get(index as usize)
            .and_then(runtime_resource_string)
            .into_iter()
            .collect(),
        ResourceSelector::Field { argument, path } => {
            let mut value = args.get(argument as usize);
            for field in path {
                value = value
                    .and_then(VmValue::as_dict)
                    .and_then(|map| map.get(*field));
            }
            value
                .and_then(runtime_resource_string)
                .into_iter()
                .collect()
        }
        ResourceSelector::EachArgument(index) => args
            .get(index as usize)
            .and_then(|value| match value {
                VmValue::List(items) => Some(items.as_slice()),
                _ => None,
            })
            .into_iter()
            .flatten()
            .filter_map(runtime_resource_string)
            .collect(),
        ResourceSelector::Constant(value) => vec![crate::value::HarnStr::from(value)],
        ResourceSelector::Dynamic => Vec::new(),
    }
}

fn runtime_effect_arguments(
    specs: &[harn_builtin_meta::EffectSpec],
    args: &[VmValue],
) -> Vec<Vec<crate::value::HarnStr>> {
    specs
        .iter()
        .flat_map(|spec| spec.resources)
        .map(|selector| resolve_runtime_resources(*selector, args))
        .collect()
}

fn runtime_effect_arguments_match(
    cached: &[Vec<crate::value::HarnStr>],
    specs: &[harn_builtin_meta::EffectSpec],
    args: &[VmValue],
) -> bool {
    let mut cached = cached.iter();
    for selector in specs.iter().flat_map(|spec| spec.resources) {
        let Some(resources) = cached.next() else {
            return false;
        };
        if !runtime_selector_resources_match(resources, *selector, args) {
            return false;
        }
    }
    cached.next().is_none()
}

fn runtime_selector_resources_match(
    cached: &[crate::value::HarnStr],
    selector: harn_builtin_meta::ResourceSelector,
    args: &[VmValue],
) -> bool {
    use harn_builtin_meta::ResourceSelector;
    match selector {
        ResourceSelector::Argument(index) => {
            runtime_scalar_resource_matches(cached, args.get(index as usize))
        }
        ResourceSelector::Field { argument, path } => {
            let mut value = args.get(argument as usize);
            for field in path {
                value = value
                    .and_then(VmValue::as_dict)
                    .and_then(|map| map.get(*field));
            }
            runtime_scalar_resource_matches(cached, value)
        }
        ResourceSelector::EachArgument(index) => {
            let values = args
                .get(index as usize)
                .and_then(|value| match value {
                    VmValue::List(items) => Some(items.as_slice()),
                    _ => None,
                })
                .into_iter()
                .flatten()
                .filter_map(|value| match value {
                    VmValue::String(value) => Some(value.as_str()),
                    _ => None,
                });
            let mut count = 0;
            for (expected, actual) in cached.iter().map(|value| value.as_str()).zip(values) {
                if expected != actual {
                    return false;
                }
                count += 1;
            }
            count == cached.len()
                && args
                    .get(index as usize)
                    .and_then(|value| match value {
                        VmValue::List(items) => Some(items.as_slice()),
                        _ => None,
                    })
                    .into_iter()
                    .flatten()
                    .filter(|value| matches!(value, VmValue::String(_)))
                    .count()
                    == cached.len()
        }
        ResourceSelector::Constant(value) => {
            cached.len() == 1
                && cached
                    .first()
                    .is_some_and(|cached| cached.as_str() == value)
        }
        ResourceSelector::Dynamic => cached.is_empty(),
    }
}

fn runtime_scalar_resource_matches(
    cached: &[crate::value::HarnStr],
    value: Option<&VmValue>,
) -> bool {
    match value {
        Some(VmValue::String(value)) => {
            cached.len() == 1
                && cached
                    .first()
                    .is_some_and(|cached| cached.as_str() == value.as_str())
        }
        _ => cached.is_empty(),
    }
}

fn runtime_resource_string(value: &VmValue) -> Option<crate::value::HarnStr> {
    match value {
        VmValue::String(value) => Some(value.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_effect_call_cache_hits_exact_calls_only() {
        static SPECS: &[harn_builtin_meta::EffectSpec] = &[harn_builtin_meta::EffectSpec::new(
            harn_builtin_meta::EffectKind::Fs,
            harn_builtin_meta::EffectAccess::Read,
            &[harn_builtin_meta::ResourceSelector::Argument(0)],
        )];
        let mut cache = RuntimeEffectCallCache::default();
        let args_a = [VmValue::String("/tmp/a".into())];
        let args_b = [VmValue::String("/tmp/b".into())];

        assert!(!cache.contains(SPECS, &args_a));
        cache.remember(SPECS, &args_a);
        assert!(cache.contains(SPECS, &args_a));
        assert!(
            !cache.contains(SPECS, &args_b),
            "distinct resource-bearing arguments must not share a cache hit"
        );
        cache.clear();
        assert!(!cache.contains(SPECS, &args_a));
    }
}
