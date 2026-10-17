use super::*;
use crate::compile_source;

#[test]
fn artifact_header_refuses_a_different_builtin_contract_set() {
    let chunk = compile_source("1 + 1").expect("compile");
    let key = CacheKey::from_source(Path::new("/tmp/contracts.harn"), "1 + 1");
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("contracts.harnbc");
    store_at(&path, &key, &chunk).expect("write");
    assert!(read_entry_candidate(&path, &key).unwrap().is_some());

    let mut foreign = key.clone();
    foreign.compiler_contracts[0] ^= 1;
    assert_ne!(key.module_filename(), foreign.module_filename());
    assert!(read_entry_candidate(&path, &foreign).unwrap().is_none());

    let module = crate::module_artifact::compile_module_artifact_from_source(
        Path::new("/tmp/contracts.harn"),
        "pub fn value() { return 1 }",
    )
    .unwrap();
    let encoded = serialize_module_artifact(&key, &module).unwrap();
    assert!(read_header_from(encoded.as_slice(), &key, Some(&key.context_hash)).is_some());
    assert!(read_header_from(encoded.as_slice(), &foreign, Some(&foreign.context_hash)).is_none());
}
