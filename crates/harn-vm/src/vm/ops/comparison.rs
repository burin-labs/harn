use crate::chunk::Op;
use crate::value::{compare_values, values_equal, VmError, VmValue};

impl super::super::Vm {
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
