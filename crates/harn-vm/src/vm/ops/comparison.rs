use std::rc::Rc;

use crate::value::{compare_values, values_equal, VmError, VmValue};

impl super::super::Vm {
    fn push_compare_result(
        &mut self,
        f: impl FnOnce(VmValue, VmValue) -> Result<bool, VmError>,
    ) -> Result<(), VmError> {
        let b = self.pop()?;
        let a = self.pop()?;
        self.stack.push(VmValue::Bool(f(a, b)?));
        Ok(())
    }

    pub(super) fn execute_equal(&mut self) -> Result<(), VmError> {
        self.push_compare_result(|a, b| Ok(values_equal(&a, &b)))
    }

    pub(super) fn execute_not_equal(&mut self) -> Result<(), VmError> {
        self.push_compare_result(|a, b| Ok(!values_equal(&a, &b)))
    }

    pub(super) fn execute_less(&mut self) -> Result<(), VmError> {
        self.push_compare_result(|a, b| Ok(compare_values(&a, &b) < 0))
    }

    pub(super) fn execute_greater(&mut self) -> Result<(), VmError> {
        self.push_compare_result(|a, b| Ok(compare_values(&a, &b) > 0))
    }

    pub(super) fn execute_less_equal(&mut self) -> Result<(), VmError> {
        self.push_compare_result(|a, b| Ok(compare_values(&a, &b) <= 0))
    }

    pub(super) fn execute_greater_equal(&mut self) -> Result<(), VmError> {
        self.push_compare_result(|a, b| Ok(compare_values(&a, &b) >= 0))
    }

    pub(super) fn execute_contains(&mut self) -> Result<(), VmError> {
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
        Ok(())
    }

    pub(super) fn execute_equal_int(&mut self) -> Result<(), VmError> {
        self.push_compare_result(|a, b| {
            let (x, y) = typed_int_pair("equal", a, b)?;
            Ok(x == y)
        })
    }

    pub(super) fn execute_not_equal_int(&mut self) -> Result<(), VmError> {
        self.push_compare_result(|a, b| {
            let (x, y) = typed_int_pair("not-equal", a, b)?;
            Ok(x != y)
        })
    }

    pub(super) fn execute_less_int(&mut self) -> Result<(), VmError> {
        self.push_compare_result(|a, b| {
            let (x, y) = typed_int_pair("less-than", a, b)?;
            Ok(x < y)
        })
    }

    pub(super) fn execute_greater_int(&mut self) -> Result<(), VmError> {
        self.push_compare_result(|a, b| {
            let (x, y) = typed_int_pair("greater-than", a, b)?;
            Ok(x > y)
        })
    }

    pub(super) fn execute_less_equal_int(&mut self) -> Result<(), VmError> {
        self.push_compare_result(|a, b| {
            let (x, y) = typed_int_pair("less-equal", a, b)?;
            Ok(x <= y)
        })
    }

    pub(super) fn execute_greater_equal_int(&mut self) -> Result<(), VmError> {
        self.push_compare_result(|a, b| {
            let (x, y) = typed_int_pair("greater-equal", a, b)?;
            Ok(x >= y)
        })
    }

    pub(super) fn execute_equal_float(&mut self) -> Result<(), VmError> {
        self.push_compare_result(|a, b| {
            let (x, y) = typed_float_pair("equal", a, b)?;
            Ok(x == y)
        })
    }

    pub(super) fn execute_not_equal_float(&mut self) -> Result<(), VmError> {
        self.push_compare_result(|a, b| {
            let (x, y) = typed_float_pair("not-equal", a, b)?;
            Ok(x != y)
        })
    }

    pub(super) fn execute_less_float(&mut self) -> Result<(), VmError> {
        self.push_compare_result(|a, b| {
            let (x, y) = typed_float_pair("less-than", a, b)?;
            Ok(x < y)
        })
    }

    pub(super) fn execute_greater_float(&mut self) -> Result<(), VmError> {
        self.push_compare_result(|a, b| {
            let (x, y) = typed_float_pair("greater-than", a, b)?;
            Ok(x > y)
        })
    }

    pub(super) fn execute_less_equal_float(&mut self) -> Result<(), VmError> {
        self.push_compare_result(|a, b| {
            let (x, y) = typed_float_pair("less-equal", a, b)?;
            Ok(x <= y)
        })
    }

    pub(super) fn execute_greater_equal_float(&mut self) -> Result<(), VmError> {
        self.push_compare_result(|a, b| {
            let (x, y) = typed_float_pair("greater-equal", a, b)?;
            Ok(x >= y)
        })
    }

    pub(super) fn execute_equal_bool(&mut self) -> Result<(), VmError> {
        self.push_compare_result(|a, b| {
            let (x, y) = typed_bool_pair("equal", a, b)?;
            Ok(x == y)
        })
    }

    pub(super) fn execute_not_equal_bool(&mut self) -> Result<(), VmError> {
        self.push_compare_result(|a, b| {
            let (x, y) = typed_bool_pair("not-equal", a, b)?;
            Ok(x != y)
        })
    }

    pub(super) fn execute_equal_string(&mut self) -> Result<(), VmError> {
        self.push_compare_result(|a, b| {
            let (x, y) = typed_string_pair("equal", a, b)?;
            Ok(x == y)
        })
    }

    pub(super) fn execute_not_equal_string(&mut self) -> Result<(), VmError> {
        self.push_compare_result(|a, b| {
            let (x, y) = typed_string_pair("not-equal", a, b)?;
            Ok(x != y)
        })
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
