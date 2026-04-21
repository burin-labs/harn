use std::rc::Rc;

use crate::chunk::Op;
use crate::value::{compare_values, values_equal, VmError, VmValue};

impl super::super::Vm {
    pub(super) fn execute_typed_comparison_op(&mut self, op: u8) -> Result<(), VmError> {
        if op == Op::LessInt as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            let (x, y) = typed_int_pair("less-than", a, b)?;
            self.stack.push(VmValue::Bool(x < y));
        } else if op == Op::GreaterEqualInt as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            let (x, y) = typed_int_pair("greater-equal", a, b)?;
            self.stack.push(VmValue::Bool(x >= y));
        } else if op == Op::LessEqualInt as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            let (x, y) = typed_int_pair("less-equal", a, b)?;
            self.stack.push(VmValue::Bool(x <= y));
        } else if op == Op::EqualInt as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            let (x, y) = typed_int_pair("equal", a, b)?;
            self.stack.push(VmValue::Bool(x == y));
        } else if op == Op::NotEqualInt as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            let (x, y) = typed_int_pair("not-equal", a, b)?;
            self.stack.push(VmValue::Bool(x != y));
        } else if op == Op::GreaterInt as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            let (x, y) = typed_int_pair("greater-than", a, b)?;
            self.stack.push(VmValue::Bool(x > y));
        } else if op == Op::EqualFloat as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            let (x, y) = typed_float_pair("equal", a, b)?;
            self.stack.push(VmValue::Bool(x == y));
        } else if op == Op::NotEqualFloat as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            let (x, y) = typed_float_pair("not-equal", a, b)?;
            self.stack.push(VmValue::Bool(x != y));
        } else if op == Op::LessFloat as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            let (x, y) = typed_float_pair("less-than", a, b)?;
            self.stack.push(VmValue::Bool(x < y));
        } else if op == Op::GreaterFloat as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            let (x, y) = typed_float_pair("greater-than", a, b)?;
            self.stack.push(VmValue::Bool(x > y));
        } else if op == Op::LessEqualFloat as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            let (x, y) = typed_float_pair("less-equal", a, b)?;
            self.stack.push(VmValue::Bool(x <= y));
        } else if op == Op::GreaterEqualFloat as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            let (x, y) = typed_float_pair("greater-equal", a, b)?;
            self.stack.push(VmValue::Bool(x >= y));
        } else if op == Op::EqualBool as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            let (x, y) = typed_bool_pair("equal", a, b)?;
            self.stack.push(VmValue::Bool(x == y));
        } else if op == Op::NotEqualBool as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            let (x, y) = typed_bool_pair("not-equal", a, b)?;
            self.stack.push(VmValue::Bool(x != y));
        } else if op == Op::EqualString as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            let (x, y) = typed_string_pair("equal", a, b)?;
            self.stack.push(VmValue::Bool(x == y));
        } else if op == Op::NotEqualString as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            let (x, y) = typed_string_pair("not-equal", a, b)?;
            self.stack.push(VmValue::Bool(x != y));
        } else {
            return Err(VmError::InvalidInstruction(op));
        }
        Ok(())
    }

    pub(super) fn try_execute_comparison_op(&mut self, op: u8) -> Result<bool, VmError> {
        if op == Op::Equal as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            self.stack.push(VmValue::Bool(values_equal(&a, &b)));
        } else if op == Op::NotEqual as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            self.stack.push(VmValue::Bool(!values_equal(&a, &b)));
        } else if op == Op::Less as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            self.stack.push(VmValue::Bool(compare_values(&a, &b) < 0));
        } else if op == Op::Greater as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            self.stack.push(VmValue::Bool(compare_values(&a, &b) > 0));
        } else if op == Op::LessEqual as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            self.stack.push(VmValue::Bool(compare_values(&a, &b) <= 0));
        } else if op == Op::GreaterEqual as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            self.stack.push(VmValue::Bool(compare_values(&a, &b) >= 0));
        } else if op == Op::Contains as u8 {
            let collection = self.pop()?;
            let item = self.pop()?;
            let result = match &collection {
                VmValue::List(items) => items.iter().any(|v| values_equal(v, &item)),
                VmValue::Dict(map) => {
                    let key = item.display();
                    map.contains_key(&key)
                }
                VmValue::Set(items) => items.iter().any(|v| values_equal(v, &item)),
                VmValue::Range(r) => match &item {
                    VmValue::Int(n) => r.contains(*n),
                    _ => false,
                },
                VmValue::String(s) => {
                    if let VmValue::String(substr) = &item {
                        s.contains(&**substr)
                    } else {
                        let substr = item.display();
                        s.contains(&substr)
                    }
                }
                _ => false,
            };
            self.stack.push(VmValue::Bool(result));
        } else {
            return Ok(false);
        }
        Ok(true)
    }
}

#[inline]
fn typed_int_pair(name: &str, a: VmValue, b: VmValue) -> Result<(i64, i64), VmError> {
    match (a, b) {
        (VmValue::Int(x), VmValue::Int(y)) => Ok((x, y)),
        (a, b) => Err(VmError::TypeError(format!(
            "Typed int {name} expected int operands, got {} and {}",
            a.type_name(),
            b.type_name()
        ))),
    }
}

#[inline]
fn typed_float_pair(name: &str, a: VmValue, b: VmValue) -> Result<(f64, f64), VmError> {
    match (a, b) {
        (VmValue::Float(x), VmValue::Float(y)) => Ok((x, y)),
        (a, b) => Err(VmError::TypeError(format!(
            "Typed float {name} expected float operands, got {} and {}",
            a.type_name(),
            b.type_name()
        ))),
    }
}

#[inline]
fn typed_bool_pair(name: &str, a: VmValue, b: VmValue) -> Result<(bool, bool), VmError> {
    match (a, b) {
        (VmValue::Bool(x), VmValue::Bool(y)) => Ok((x, y)),
        (a, b) => Err(VmError::TypeError(format!(
            "Typed bool {name} expected bool operands, got {} and {}",
            a.type_name(),
            b.type_name()
        ))),
    }
}

#[inline]
fn typed_string_pair(name: &str, a: VmValue, b: VmValue) -> Result<(Rc<str>, Rc<str>), VmError> {
    match (a, b) {
        (VmValue::String(x), VmValue::String(y)) => Ok((x, y)),
        (a, b) => Err(VmError::TypeError(format!(
            "Typed string {name} expected string operands, got {} and {}",
            a.type_name(),
            b.type_name()
        ))),
    }
}
