use super::cost::parse_budget;
use crate::value::{intern_key, DictMap, VmValue};

#[test]
fn budget_number_shorthand_sets_max_cost_usd() {
    let options = DictMap::from_iter([(intern_key("budget"), VmValue::Float(0.25))]);
    let envelope = parse_budget(Some(&options))
        .expect("valid budget")
        .expect("non-empty envelope");
    assert_eq!(envelope.max_cost_usd, Some(0.25));
    assert_eq!(envelope.total_budget_usd, None);
}

#[test]
fn budget_dict_parses_named_fields() {
    let fields = DictMap::from_iter([
        (intern_key("max_cost_usd"), VmValue::Float(1.5)),
        (intern_key("total_budget_usd"), VmValue::Float(10.0)),
    ]);
    let options = DictMap::from_iter([(intern_key("budget"), VmValue::dict(fields))]);
    let envelope = parse_budget(Some(&options))
        .expect("valid budget")
        .expect("non-empty envelope");
    assert_eq!(envelope.max_cost_usd, Some(1.5));
    assert_eq!(envelope.total_budget_usd, Some(10.0));
}

#[test]
fn flat_top_level_budget_fields_are_not_read() {
    let options = DictMap::from_iter([(intern_key("max_cost_usd"), VmValue::Float(3.0))]);
    assert!(parse_budget(Some(&options))
        .expect("no budget key")
        .is_none());
}

#[test]
fn conservative_admission_requires_a_total_and_rejects_unknown_modes() {
    for (mode, total, valid) in [
        ("conservative", Some(0.6), true),
        ("conservative", None, false),
        ("estimated", Some(0.6), false),
    ] {
        let mut fields =
            DictMap::from_iter([(intern_key("admission"), VmValue::String(mode.into()))]);
        if let Some(total) = total {
            fields.insert(intern_key("total_budget_usd"), VmValue::Float(total));
        }
        let options = DictMap::from_iter([(intern_key("budget"), VmValue::dict(fields))]);
        assert_eq!(
            parse_budget(Some(&options)).is_ok(),
            valid,
            "{mode} {total:?}"
        );
    }
}
