use crate::CapabilityId;

/// Runtime discriminator for the root grant or one typed capability handle.
/// The capability registry owns the field order, names, and source types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HarnessKind(Option<CapabilityId>);

macro_rules! define_capability_kinds {
    (@constant $variant:ident) => { pub const $variant: Self = Self(Some(CapabilityId::$variant)); };
    (@constant $variant:ident as $kind:ident) => { pub const $kind: Self = Self(Some(CapabilityId::$variant)); };
    ($($variant:ident $(as $kind:ident)? => ($field:literal, $type:literal)),* $(,)?) => {
        #[allow(non_upper_case_globals)]
        impl HarnessKind { $(define_capability_kinds!(@constant $variant $(as $kind)?);)* }
    };
}

crate::capability_registry::capability_registry!(define_capability_kinds);

#[allow(non_upper_case_globals)]
impl HarnessKind {
    pub const Root: Self = Self(None);

    pub const fn capability_id(self) -> Option<CapabilityId> {
        self.0
    }

    pub const fn type_name(self) -> &'static str {
        match self.0 {
            None => "Harness",
            Some(capability) => capability.type_name(),
        }
    }

    pub const fn field_name(self) -> Option<&'static str> {
        match self.0 {
            None => None,
            Some(capability) => Some(capability.field_name()),
        }
    }

    pub const fn from_field_name(name: &str) -> Option<Self> {
        match CapabilityId::from_field_name(name) {
            Some(capability) => Some(Self(Some(capability))),
            None => None,
        }
    }

    /// All sub-handles in canonical root-field order.
    pub const SUB_HANDLES: &'static [Self] = &Self::sub_handles();

    /// Every kind a Harn-script type annotation may reference.
    pub const ALL: &'static [Self] = &Self::all();

    const fn sub_handles() -> [Self; CapabilityId::ALL.len()] {
        let mut kinds = [Self::Root; CapabilityId::ALL.len()];
        let mut index = 0;
        while index < kinds.len() {
            kinds[index] = Self(Some(CapabilityId::ALL[index]));
            index += 1;
        }
        kinds
    }

    const fn all() -> [Self; CapabilityId::ALL.len() + 1] {
        let mut kinds = [Self::Root; CapabilityId::ALL.len() + 1];
        let mut index = 0;
        while index < CapabilityId::ALL.len() {
            kinds[index + 1] = Self(Some(CapabilityId::ALL[index]));
            index += 1;
        }
        kinds
    }
}
