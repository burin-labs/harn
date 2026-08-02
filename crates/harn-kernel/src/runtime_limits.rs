pub struct RuntimeLimits {
    pub max_constant_folded_collection_items: usize,
    pub max_constant_folded_string_bytes: usize,
}

impl RuntimeLimits {
    pub const DEFAULT: Self = Self {
        max_constant_folded_collection_items: 4_096,
        max_constant_folded_string_bytes: 64 * 1024,
    };
}
