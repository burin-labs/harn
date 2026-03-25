use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use crate::error::RuntimeError;
use crate::value::Value;

/// Lexical scoping environment with parent chain.
#[derive(Debug, Clone)]
pub struct Environment {
    inner: Rc<RefCell<EnvInner>>,
}

#[derive(Debug)]
struct EnvInner {
    values: HashMap<String, Value>,
    mutable: HashSet<String>,
    parent: Option<Environment>,
}

impl Environment {
    pub fn new() -> Self {
        Self {
            inner: Rc::new(RefCell::new(EnvInner {
                values: HashMap::new(),
                mutable: HashSet::new(),
                parent: None,
            })),
        }
    }

    pub fn child(&self) -> Self {
        Self {
            inner: Rc::new(RefCell::new(EnvInner {
                values: HashMap::new(),
                mutable: HashSet::new(),
                parent: Some(self.clone()),
            })),
        }
    }

    pub fn get(&self, name: &str) -> Option<Value> {
        let inner = self.inner.borrow();
        if let Some(val) = inner.values.get(name) {
            Some(val.clone())
        } else if let Some(ref parent) = inner.parent {
            parent.get(name)
        } else {
            None
        }
    }

    pub fn define(&self, name: &str, value: Value, is_mutable: bool) {
        let mut inner = self.inner.borrow_mut();
        inner.values.insert(name.to_string(), value);
        if is_mutable {
            inner.mutable.insert(name.to_string());
        }
    }

    pub fn assign(&self, name: &str, value: Value) -> Result<(), RuntimeError> {
        let mut inner = self.inner.borrow_mut();
        if inner.values.contains_key(name) {
            if !inner.mutable.contains(name) {
                return Err(RuntimeError::ImmutableAssignment(name.to_string()));
            }
            inner.values.insert(name.to_string(), value);
            return Ok(());
        }
        if let Some(ref parent) = inner.parent {
            let parent = parent.clone();
            drop(inner); // release borrow before recursive call
            parent.assign(name, value)
        } else {
            Err(RuntimeError::UndefinedVariable(name.to_string()))
        }
    }
}

impl Default for Environment {
    fn default() -> Self {
        Self::new()
    }
}
