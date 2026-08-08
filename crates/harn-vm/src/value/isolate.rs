use super::{VmError, VmValue};

/// A data value proven safe to instantiate in independent VM isolates.
///
/// The wrapped representation stays private so callers cannot bypass the
/// one-time graph validation. Cloning an instance is O(1) at this boundary;
/// Harn collections preserve isolation with copy-on-write mutation.
#[derive(Clone)]
pub struct IsolateValue(VmValue);

impl IsolateValue {
    /// Create one copy-on-write instance for a fresh VM.
    pub fn instantiate(&self) -> VmValue {
        self.0.clone()
    }
}

impl VmValue {
    /// Validate this value as reusable data for fresh VM isolates.
    ///
    /// The complete value graph is validated once, rejecting execution-bound
    /// handles whose clone would retain source-VM state. The returned opaque
    /// seed can then instantiate isolated values without rescanning the graph.
    ///
    /// Validation is iterative so adversarially deep data cannot overflow the
    /// native stack.
    pub fn try_into_isolate_value(self) -> Result<IsolateValue, VmError> {
        let mut pending = vec![&self];
        while let Some(value) = pending.pop() {
            match value {
                Self::List(items) => pending.extend(items.iter()),
                Self::Dict(entries) => pending.extend(entries.values()),
                Self::EnumVariant(variant) => pending.extend(variant.fields.iter()),
                Self::StructInstance(instance) => {
                    pending.extend(instance.fields.iter().filter_map(Option::as_ref));
                }
                Self::Set(set) => pending.extend(set.iter()),
                Self::Pair(pair) => {
                    pending.push(&pair.0);
                    pending.push(&pair.1);
                }
                Self::Closure(_)
                | Self::TaskHandle(_)
                | Self::Channel(_)
                | Self::Atomic(_)
                | Self::Rng(_)
                | Self::SyncPermit(_)
                | Self::Resource(_)
                | Self::ResourceGuard(_)
                | Self::McpClient(_)
                | Self::VerdictReceipt(_)
                | Self::Generator(_)
                | Self::Stream(_)
                | Self::Iter(_)
                | Self::Harness(_) => {
                    return Err(VmError::Runtime(format!(
                        "{} values retain execution state and cannot cross a VM isolate",
                        value.type_name()
                    )));
                }
                Self::Int(_)
                | Self::Float(_)
                | Self::Decimal(_)
                | Self::String(_)
                | Self::Bytes(_)
                | Self::Bool(_)
                | Self::Nil
                | Self::BuiltinRef(_)
                | Self::BuiltinRefId(_)
                | Self::Duration(_)
                | Self::Range(_) => {}
            }
        }
        Ok(IsolateValue(self))
    }
}
