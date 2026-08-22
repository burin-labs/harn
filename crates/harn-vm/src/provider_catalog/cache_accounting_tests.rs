use super::*;

#[test]
fn swift_binding_defaults_pre_v7_cache_accounting_to_unsupported() {
    let swift = swift_binding().expect("Swift binding renders");
    assert!(swift.contains("private let encodedCacheUsageAccounting: Bool?"));
    assert!(swift.contains(
        "public var cacheUsageAccounting: Bool { encodedCacheUsageAccounting ?? false }"
    ));
}
