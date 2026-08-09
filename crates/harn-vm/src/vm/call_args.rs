use crate::value::VmValue;

/// Borrowed callable arguments for VM-internal dispatch.
///
/// The hot callback path is dominated by zero-, one-, and two-argument calls.
/// Keeping those shapes explicit lets closure binding and sync builtin dispatch
/// avoid materializing a temporary `Vec<VmValue>`.
pub(crate) enum CallArgs<'a> {
    Empty,
    One(&'a VmValue),
    Two(&'a VmValue, &'a VmValue),
    Slice(&'a [VmValue]),
    Owned(Vec<VmValue>),
}

impl<'a> CallArgs<'a> {
    pub(crate) fn len(&self) -> usize {
        match self {
            Self::Empty => 0,
            Self::One(_) => 1,
            Self::Two(_, _) => 2,
            Self::Slice(args) => args.len(),
            Self::Owned(args) => args.len(),
        }
    }

    pub(crate) fn get(&self, index: usize) -> Option<&VmValue> {
        match self {
            Self::Empty => None,
            Self::One(arg) => (index == 0).then_some(*arg),
            Self::Two(first, second) => match index {
                0 => Some(*first),
                1 => Some(*second),
                _ => None,
            },
            Self::Slice(args) => args.get(index),
            Self::Owned(args) => args.get(index),
        }
    }

    pub(crate) fn iter(&self) -> CallArgsIter<'_> {
        CallArgsIter {
            args: self,
            index: 0,
        }
    }

    pub(crate) fn into_vec(self) -> Vec<VmValue> {
        match self {
            Self::Empty => Vec::new(),
            Self::One(arg) => vec![arg.clone()],
            Self::Two(first, second) => vec![first.clone(), second.clone()],
            Self::Slice(args) => args.to_vec(),
            Self::Owned(args) => args,
        }
    }

    pub(crate) fn to_vec_from(&self, start: usize) -> Vec<VmValue> {
        match self {
            Self::Empty | Self::One(_) | Self::Two(_, _) => {
                self.iter().skip(start).cloned().collect()
            }
            Self::Slice(args) => args.get(start..).unwrap_or(&[]).to_vec(),
            Self::Owned(args) => args.get(start..).unwrap_or(&[]).to_vec(),
        }
    }

    pub(crate) fn with_slice<R>(&self, f: impl FnOnce(&[VmValue]) -> R) -> R {
        match self {
            Self::Empty => f(&[]),
            Self::One(arg) => f(std::slice::from_ref(*arg)),
            Self::Two(first, second) => {
                let args = [(*first).clone(), (*second).clone()];
                f(&args)
            }
            Self::Slice(args) => f(args),
            Self::Owned(args) => f(args),
        }
    }
}

pub(crate) struct CallArgsIter<'a> {
    args: &'a CallArgs<'a>,
    index: usize,
}

impl<'a> Iterator for CallArgsIter<'a> {
    type Item = &'a VmValue;

    fn next(&mut self) -> Option<Self::Item> {
        let value = self.args.get(self.index);
        self.index += usize::from(value.is_some());
        value
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.args.len().saturating_sub(self.index);
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for CallArgsIter<'_> {}
