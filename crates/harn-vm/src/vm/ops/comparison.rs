use crate::chunk::AdaptiveBinaryOp;
use crate::value::{values_equal, VmError, VmValue};

impl super::super::Vm {
    pub(super) fn execute_equal(&mut self) -> Result<(), VmError> {
        self.execute_adaptive_binary(AdaptiveBinaryOp::Equal)
    }

    pub(super) fn execute_not_equal(&mut self) -> Result<(), VmError> {
        self.execute_adaptive_binary(AdaptiveBinaryOp::NotEqual)
    }

    pub(super) fn execute_less(&mut self) -> Result<(), VmError> {
        self.execute_adaptive_binary(AdaptiveBinaryOp::Less)
    }

    pub(super) fn execute_greater(&mut self) -> Result<(), VmError> {
        self.execute_adaptive_binary(AdaptiveBinaryOp::Greater)
    }

    pub(super) fn execute_less_equal(&mut self) -> Result<(), VmError> {
        self.execute_adaptive_binary(AdaptiveBinaryOp::LessEqual)
    }

    pub(super) fn execute_greater_equal(&mut self) -> Result<(), VmError> {
        self.execute_adaptive_binary(AdaptiveBinaryOp::GreaterEqual)
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
        self.run_typed_binary(AdaptiveBinaryOp::Equal, |a, b| match (a, b) {
            (VmValue::Int(x), VmValue::Int(y)) => Some(Ok(VmValue::Bool(x == y))),
            _ => None,
        })
    }

    pub(super) fn execute_not_equal_int(&mut self) -> Result<(), VmError> {
        self.run_typed_binary(AdaptiveBinaryOp::NotEqual, |a, b| match (a, b) {
            (VmValue::Int(x), VmValue::Int(y)) => Some(Ok(VmValue::Bool(x != y))),
            _ => None,
        })
    }

    pub(super) fn execute_less_int(&mut self) -> Result<(), VmError> {
        self.run_typed_binary(AdaptiveBinaryOp::Less, |a, b| match (a, b) {
            (VmValue::Int(x), VmValue::Int(y)) => Some(Ok(VmValue::Bool(x < y))),
            _ => None,
        })
    }

    pub(super) fn execute_greater_int(&mut self) -> Result<(), VmError> {
        self.run_typed_binary(AdaptiveBinaryOp::Greater, |a, b| match (a, b) {
            (VmValue::Int(x), VmValue::Int(y)) => Some(Ok(VmValue::Bool(x > y))),
            _ => None,
        })
    }

    pub(super) fn execute_less_equal_int(&mut self) -> Result<(), VmError> {
        self.run_typed_binary(AdaptiveBinaryOp::LessEqual, |a, b| match (a, b) {
            (VmValue::Int(x), VmValue::Int(y)) => Some(Ok(VmValue::Bool(x <= y))),
            _ => None,
        })
    }

    pub(super) fn execute_greater_equal_int(&mut self) -> Result<(), VmError> {
        self.run_typed_binary(AdaptiveBinaryOp::GreaterEqual, |a, b| match (a, b) {
            (VmValue::Int(x), VmValue::Int(y)) => Some(Ok(VmValue::Bool(x >= y))),
            _ => None,
        })
    }

    pub(super) fn execute_equal_float(&mut self) -> Result<(), VmError> {
        self.run_typed_binary(AdaptiveBinaryOp::Equal, |a, b| match (a, b) {
            (VmValue::Float(x), VmValue::Float(y)) => Some(Ok(VmValue::Bool(x == y))),
            _ => None,
        })
    }

    pub(super) fn execute_not_equal_float(&mut self) -> Result<(), VmError> {
        self.run_typed_binary(AdaptiveBinaryOp::NotEqual, |a, b| match (a, b) {
            (VmValue::Float(x), VmValue::Float(y)) => Some(Ok(VmValue::Bool(x != y))),
            _ => None,
        })
    }

    pub(super) fn execute_less_float(&mut self) -> Result<(), VmError> {
        self.run_typed_binary(AdaptiveBinaryOp::Less, |a, b| match (a, b) {
            (VmValue::Float(x), VmValue::Float(y)) => Some(Ok(VmValue::Bool(x < y))),
            _ => None,
        })
    }

    pub(super) fn execute_greater_float(&mut self) -> Result<(), VmError> {
        self.run_typed_binary(AdaptiveBinaryOp::Greater, |a, b| match (a, b) {
            (VmValue::Float(x), VmValue::Float(y)) => Some(Ok(VmValue::Bool(x > y))),
            _ => None,
        })
    }

    pub(super) fn execute_less_equal_float(&mut self) -> Result<(), VmError> {
        self.run_typed_binary(AdaptiveBinaryOp::LessEqual, |a, b| match (a, b) {
            (VmValue::Float(x), VmValue::Float(y)) => Some(Ok(VmValue::Bool(x <= y))),
            _ => None,
        })
    }

    pub(super) fn execute_greater_equal_float(&mut self) -> Result<(), VmError> {
        self.run_typed_binary(AdaptiveBinaryOp::GreaterEqual, |a, b| match (a, b) {
            (VmValue::Float(x), VmValue::Float(y)) => Some(Ok(VmValue::Bool(x >= y))),
            _ => None,
        })
    }

    pub(super) fn execute_equal_bool(&mut self) -> Result<(), VmError> {
        self.run_typed_binary(AdaptiveBinaryOp::Equal, |a, b| match (a, b) {
            (VmValue::Bool(x), VmValue::Bool(y)) => Some(Ok(VmValue::Bool(x == y))),
            _ => None,
        })
    }

    pub(super) fn execute_not_equal_bool(&mut self) -> Result<(), VmError> {
        self.run_typed_binary(AdaptiveBinaryOp::NotEqual, |a, b| match (a, b) {
            (VmValue::Bool(x), VmValue::Bool(y)) => Some(Ok(VmValue::Bool(x != y))),
            _ => None,
        })
    }

    pub(super) fn execute_equal_string(&mut self) -> Result<(), VmError> {
        self.run_typed_binary(AdaptiveBinaryOp::Equal, |a, b| match (a, b) {
            (VmValue::String(x), VmValue::String(y)) => Some(Ok(VmValue::Bool(x == y))),
            _ => None,
        })
    }

    pub(super) fn execute_not_equal_string(&mut self) -> Result<(), VmError> {
        self.run_typed_binary(AdaptiveBinaryOp::NotEqual, |a, b| match (a, b) {
            (VmValue::String(x), VmValue::String(y)) => Some(Ok(VmValue::Bool(x != y))),
            _ => None,
        })
    }
}
