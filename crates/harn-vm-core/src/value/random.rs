use rand::{seq::SliceRandom, Rng, RngExt};

use super::{args::Args, VmError, VmRngHandle, VmValue};

pub enum RandomOperation {
    Float,
    Range,
    Choice,
    Shuffle,
}

impl RandomOperation {
    pub fn apply<R: Rng + ?Sized>(self, rng: &mut R, args: &[VmValue]) -> Result<VmValue, VmError> {
        match self {
            Self::Float => Ok(VmValue::Float(rng.random())),
            Self::Range => {
                if args.len() < 2 {
                    return Ok(VmValue::Nil);
                }
                let bounds = Args::new("random_int", args);
                let min = bounds.int(0, "min")?;
                let max = bounds.int(1, "max")?;
                Ok(if min > max {
                    VmValue::Nil
                } else {
                    VmValue::Int(rng.random_range(min..=max))
                })
            }
            Self::Choice => {
                let Some(VmValue::List(items)) = args.first() else {
                    return Ok(VmValue::Nil);
                };
                Ok(if items.is_empty() {
                    VmValue::Nil
                } else {
                    items[rng.random_range(0..items.len())].clone()
                })
            }
            Self::Shuffle => {
                let Some(VmValue::List(items)) = args.first() else {
                    return Ok(VmValue::Nil);
                };
                let mut shuffled = items.as_ref().clone();
                shuffled.shuffle(rng);
                Ok(VmValue::list(shuffled))
            }
        }
    }
}

impl VmRngHandle {
    pub fn call_method(&self, method: &str, args: &[VmValue]) -> Option<Result<VmValue, VmError>> {
        let operation = match method {
            "f64" => RandomOperation::Float,
            "range" => RandomOperation::Range,
            "choice" => RandomOperation::Choice,
            "shuffle" => RandomOperation::Shuffle,
            _ => return None,
        };
        Some(operation.apply(&mut *self.rng.lock(), args))
    }
}
