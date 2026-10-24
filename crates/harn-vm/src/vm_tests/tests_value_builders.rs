use crate::value::{intern_key, HarnStr};
use crate::VmValue;
use std::sync::Arc;

#[test]
fn dict_fixture_constructor_preserves_order_and_last_duplicate_value() {
    let entries = || {
        [
            ("z".to_string(), VmValue::Int(1)),
            ("a".to_string(), VmValue::Int(2)),
            ("z".to_string(), VmValue::Int(3)),
        ]
    };
    let direct = VmValue::dict(entries());
    let via_tree = VmValue::dict(entries());
    let via_persistent = VmValue::dict(entries());
    assert_eq!(direct.display(), via_tree.display());
    assert_eq!(direct.display(), via_persistent.display());
    let map = direct.as_dict().expect("dict fixture");
    assert_eq!(
        map.keys().map(HarnStr::as_str).collect::<Vec<_>>(),
        ["a", "z"]
    );
    assert!(matches!(map.get("z"), Some(VmValue::Int(3))));
}

#[test]
fn dict_fixture_constructor_retains_shared_keys_and_nested_value_storage() {
    let key = intern_key("fixture_shared_value");
    let text = HarnStr::from("fixture text");
    let nested = Arc::new(vec![VmValue::String(text.clone())]);
    let direct = VmValue::dict([(key.clone(), VmValue::List(nested.clone()))]);
    let via_persistent = VmValue::dict([(key.clone(), VmValue::List(nested.clone()))]);
    for fixture in [direct, via_persistent] {
        let map = fixture.as_dict().expect("dict fixture");
        let (stored_key, VmValue::List(stored)) = map.iter().next().expect("one entry") else {
            panic!("expected nested list");
        };
        assert!(HarnStr::ptr_eq(&key, stored_key));
        assert!(Arc::ptr_eq(&nested, stored));
        let VmValue::String(stored_text) = &stored[0] else {
            panic!("expected nested string");
        };
        assert!(HarnStr::ptr_eq(&text, stored_text));
    }
}
