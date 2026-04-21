use std::rc::Rc;

use crate::chunk::Op;
use crate::value::{VmError, VmValue};

impl super::super::Vm {
    pub(super) fn execute_typed_arithmetic_op(&mut self, op: u8) -> Result<(), VmError> {
        if op == Op::AddInt as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            let (x, y) = typed_int_pair("add", a, b)?;
            self.stack.push(VmValue::Int(x.wrapping_add(y)));
        } else if op == Op::SubInt as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            let (x, y) = typed_int_pair("subtract", a, b)?;
            self.stack.push(VmValue::Int(x.wrapping_sub(y)));
        } else if op == Op::MulInt as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            let (x, y) = typed_int_pair("multiply", a, b)?;
            self.stack.push(VmValue::Int(x.wrapping_mul(y)));
        } else if op == Op::DivInt as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            let (x, y) = typed_int_pair("divide", a, b)?;
            if y == 0 {
                return Err(VmError::DivisionByZero);
            }
            self.stack.push(VmValue::Int(x / y));
        } else if op == Op::ModInt as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            let (x, y) = typed_int_pair("modulo", a, b)?;
            if y == 0 {
                return Err(VmError::DivisionByZero);
            }
            self.stack.push(VmValue::Int(x % y));
        } else if op == Op::AddFloat as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            let (x, y) = typed_float_pair("add", a, b)?;
            self.stack.push(VmValue::Float(x + y));
        } else if op == Op::SubFloat as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            let (x, y) = typed_float_pair("subtract", a, b)?;
            self.stack.push(VmValue::Float(x - y));
        } else if op == Op::MulFloat as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            let (x, y) = typed_float_pair("multiply", a, b)?;
            self.stack.push(VmValue::Float(x * y));
        } else if op == Op::DivFloat as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            let (x, y) = typed_float_pair("divide", a, b)?;
            self.stack.push(VmValue::Float(x / y));
        } else if op == Op::ModFloat as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            let (x, y) = typed_float_pair("modulo", a, b)?;
            if y == 0.0 {
                return Err(VmError::DivisionByZero);
            }
            self.stack.push(VmValue::Float(x % y));
        } else {
            return Err(VmError::InvalidInstruction(op));
        }
        Ok(())
    }

    pub(super) fn try_execute_arithmetic_op(&mut self, op: u8) -> Result<bool, VmError> {
        if op == Op::Add as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            self.stack.push(self.add(a, b)?);
        } else if op == Op::Sub as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            self.stack.push(self.sub(a, b)?);
        } else if op == Op::Mul as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            self.stack.push(self.mul(a, b)?);
        } else if op == Op::Div as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            self.stack.push(self.div(a, b)?);
        } else if op == Op::Mod as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            self.stack.push(self.modulo(a, b)?);
        } else if op == Op::Pow as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            self.stack.push(self.pow(a, b)?);
        } else if op == Op::Negate as u8 {
            let v = self.pop()?;
            self.stack.push(match v {
                VmValue::Int(n) => VmValue::Int(n.wrapping_neg()),
                VmValue::Float(n) => VmValue::Float(-n),
                _ => {
                    return Err(VmError::Runtime(format!(
                        "Cannot negate value of type {}",
                        v.type_name()
                    )))
                }
            });
        } else {
            return Ok(false);
        }
        Ok(true)
    }

    fn add(&self, a: VmValue, b: VmValue) -> Result<VmValue, VmError> {
        match (&a, &b) {
            (VmValue::Int(x), VmValue::Int(y)) => Ok(VmValue::Int(x.wrapping_add(*y))),
            (VmValue::Float(x), VmValue::Float(y)) => Ok(VmValue::Float(x + y)),
            (VmValue::Int(x), VmValue::Float(y)) => Ok(VmValue::Float(*x as f64 + y)),
            (VmValue::Float(x), VmValue::Int(y)) => Ok(VmValue::Float(x + *y as f64)),
            (VmValue::String(x), VmValue::String(y)) => {
                let mut s = String::with_capacity(x.len() + y.len());
                s.push_str(x);
                s.push_str(y);
                Ok(VmValue::String(Rc::from(s)))
            }
            (VmValue::List(x), VmValue::List(y)) => {
                let mut result = Vec::with_capacity(x.len() + y.len());
                result.extend(x.iter().cloned());
                result.extend(y.iter().cloned());
                Ok(VmValue::List(Rc::new(result)))
            }
            (VmValue::Dict(x), VmValue::Dict(y)) => {
                let mut result = (**x).clone();
                result.extend(y.iter().map(|(k, v)| (k.clone(), v.clone())));
                Ok(VmValue::Dict(Rc::new(result)))
            }
            _ => Err(VmError::TypeError(format!(
                "Cannot add {} and {}",
                a.type_name(),
                b.type_name()
            ))),
        }
    }

    fn sub(&self, a: VmValue, b: VmValue) -> Result<VmValue, VmError> {
        match (&a, &b) {
            (VmValue::Int(x), VmValue::Int(y)) => Ok(VmValue::Int(x.wrapping_sub(*y))),
            (VmValue::Float(x), VmValue::Float(y)) => Ok(VmValue::Float(x - y)),
            (VmValue::Int(x), VmValue::Float(y)) => Ok(VmValue::Float(*x as f64 - y)),
            (VmValue::Float(x), VmValue::Int(y)) => Ok(VmValue::Float(x - *y as f64)),
            _ => Err(VmError::TypeError(format!(
                "Cannot subtract {} from {}",
                b.type_name(),
                a.type_name()
            ))),
        }
    }

    fn mul(&self, a: VmValue, b: VmValue) -> Result<VmValue, VmError> {
        match (&a, &b) {
            (VmValue::Int(x), VmValue::Int(y)) => Ok(VmValue::Int(x.wrapping_mul(*y))),
            (VmValue::Float(x), VmValue::Float(y)) => Ok(VmValue::Float(x * y)),
            (VmValue::Int(x), VmValue::Float(y)) => Ok(VmValue::Float(*x as f64 * y)),
            (VmValue::Float(x), VmValue::Int(y)) => Ok(VmValue::Float(x * *y as f64)),
            (VmValue::String(s), VmValue::Int(n)) | (VmValue::Int(n), VmValue::String(s)) => {
                let count = (*n).max(0) as usize;
                Ok(VmValue::String(s.repeat(count).into()))
            }
            _ => Err(VmError::TypeError(format!(
                "Cannot multiply {} and {}",
                a.type_name(),
                b.type_name()
            ))),
        }
    }

    fn div(&self, a: VmValue, b: VmValue) -> Result<VmValue, VmError> {
        match (&a, &b) {
            (VmValue::Int(_), VmValue::Int(y)) if *y == 0 => Err(VmError::DivisionByZero),
            (VmValue::Int(x), VmValue::Int(y)) => Ok(VmValue::Int(x / y)),
            (VmValue::Float(x), VmValue::Float(y)) => Ok(VmValue::Float(x / y)),
            (VmValue::Int(x), VmValue::Float(y)) => Ok(VmValue::Float(*x as f64 / y)),
            (VmValue::Float(x), VmValue::Int(y)) => Ok(VmValue::Float(x / *y as f64)),
            _ => Err(VmError::Runtime(format!(
                "Cannot divide {} by {}",
                a.type_name(),
                b.type_name()
            ))),
        }
    }

    fn modulo(&self, a: VmValue, b: VmValue) -> Result<VmValue, VmError> {
        match (&a, &b) {
            (VmValue::Int(_), VmValue::Int(y)) if *y == 0 => Err(VmError::DivisionByZero),
            (VmValue::Int(x), VmValue::Int(y)) => Ok(VmValue::Int(x % y)),
            (VmValue::Float(_), VmValue::Float(y)) if *y == 0.0 => Err(VmError::DivisionByZero),
            (VmValue::Float(x), VmValue::Float(y)) => Ok(VmValue::Float(x % y)),
            (VmValue::Int(_), VmValue::Float(y)) if *y == 0.0 => Err(VmError::DivisionByZero),
            (VmValue::Int(x), VmValue::Float(y)) => Ok(VmValue::Float(*x as f64 % y)),
            (VmValue::Float(_), VmValue::Int(y)) if *y == 0 => Err(VmError::DivisionByZero),
            (VmValue::Float(x), VmValue::Int(y)) => Ok(VmValue::Float(x % *y as f64)),
            _ => Err(VmError::Runtime(format!(
                "Cannot modulo {} by {}",
                a.type_name(),
                b.type_name()
            ))),
        }
    }

    fn pow(&self, a: VmValue, b: VmValue) -> Result<VmValue, VmError> {
        match (&a, &b) {
            (VmValue::Int(base), VmValue::Int(exp)) => {
                if *exp >= 0 && *exp <= u32::MAX as i64 {
                    Ok(VmValue::Int(base.wrapping_pow(*exp as u32)))
                } else {
                    Ok(VmValue::Float((*base as f64).powf(*exp as f64)))
                }
            }
            (VmValue::Float(base), VmValue::Int(exp)) => {
                if *exp >= i32::MIN as i64 && *exp <= i32::MAX as i64 {
                    Ok(VmValue::Float(base.powi(*exp as i32)))
                } else {
                    Ok(VmValue::Float(base.powf(*exp as f64)))
                }
            }
            (VmValue::Int(base), VmValue::Float(exp)) => {
                Ok(VmValue::Float((*base as f64).powf(*exp)))
            }
            (VmValue::Float(base), VmValue::Float(exp)) => Ok(VmValue::Float(base.powf(*exp))),
            _ => Err(VmError::TypeError(format!(
                "Cannot exponentiate {} by {}",
                a.type_name(),
                b.type_name()
            ))),
        }
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
