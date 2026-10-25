use super::*;
use crate::vm::Vm;

fn vm() -> Vm {
    let mut vm = crate::new_vm();
    register_concurrency_builtins(&mut vm);
    vm
}

fn call(vm: &mut Vm, name: &str, args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let f = vm.builtin_handler_for_test(name).unwrap().clone();
    let mut out = String::new();
    f(&args, &mut out)
}

fn s(v: &str) -> VmValue {
    VmValue::string(v)
}

#[test]
fn channel_rejects_invalid_capacity() {
    let mut vm = vm();
    let err = call(&mut vm, "channel", vec![s("bad_channel"), VmValue::Int(0)])
        .expect_err("zero capacity must fail");
    assert!(err.to_string().contains("capacity"));
}

#[test]
fn closed_empty_channel_counts_as_closed_for_select() {
    let mut vm = vm();
    let channel = call(&mut vm, "channel", vec![s("done")]).unwrap();
    call(&mut vm, "close_channel", vec![channel.clone()]).unwrap();
    let (ready, all_closed) = try_poll_channels(&[channel]);
    assert!(ready.is_none());
    assert!(all_closed);
}

#[test]
fn atomic_default_zero() {
    let mut vm = vm();
    let atom = call(&mut vm, "atomic", vec![]).unwrap();
    let val = call(&mut vm, "atomic_get", vec![atom]).unwrap();
    assert_eq!(val.display(), "0");
}

#[test]
fn atomic_initial_value() {
    let mut vm = vm();
    let atom = call(&mut vm, "atomic", vec![VmValue::Int(42)]).unwrap();
    let val = call(&mut vm, "atomic_get", vec![atom]).unwrap();
    assert_eq!(val.display(), "42");
}

#[test]
fn atomic_set_returns_old() {
    let mut vm = vm();
    let atom = call(&mut vm, "atomic", vec![VmValue::Int(10)]).unwrap();
    let old = call(&mut vm, "atomic_set", vec![atom.clone(), VmValue::Int(20)]).unwrap();
    assert_eq!(old.display(), "10");
    let cur = call(&mut vm, "atomic_get", vec![atom]).unwrap();
    assert_eq!(cur.display(), "20");
}

#[test]
fn atomic_add() {
    let mut vm = vm();
    let atom = call(&mut vm, "atomic", vec![VmValue::Int(5)]).unwrap();
    let prev = call(&mut vm, "atomic_add", vec![atom.clone(), VmValue::Int(3)]).unwrap();
    assert_eq!(prev.display(), "5");
    let cur = call(&mut vm, "atomic_get", vec![atom]).unwrap();
    assert_eq!(cur.display(), "8");
}

#[test]
fn atomic_bool_init() {
    let mut vm = vm();
    let atom = call(&mut vm, "atomic", vec![VmValue::Bool(true)]).unwrap();
    let val = call(&mut vm, "atomic_get", vec![atom]).unwrap();
    assert_eq!(val.display(), "1");
}

#[test]
fn timer_end_returns_elapsed() {
    let mut vm = vm();
    let timer = call(&mut vm, "timer_start", vec![s("t")]).unwrap();
    let elapsed = call(&mut vm, "timer_end", vec![timer]).unwrap();
    assert!(elapsed.as_int().unwrap() >= 0);
    assert!(elapsed.as_int().unwrap() < 1000);
}

#[test]
fn timer_end_non_dict_errors() {
    let mut vm = vm();
    let result = call(&mut vm, "timer_end", vec![VmValue::Int(42)]);
    assert!(result.is_err());
}
