use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use crate::error::RuntimeError;
use crate::value::Value;

/// Lexical scoping environment with parent chain.
/// Uses Arc<Mutex<>> for thread safety (required by tokio tasks).
#[derive(Debug, Clone)]
pub struct Environment {
    inner: Arc<Mutex<EnvInner>>,
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
            inner: Arc::new(Mutex::new(EnvInner {
                values: HashMap::new(),
                mutable: HashSet::new(),
                parent: None,
            })),
        }
    }

    pub fn child(&self) -> Self {
        Self {
            inner: Arc::new(Mutex::new(EnvInner {
                values: HashMap::new(),
                mutable: HashSet::new(),
                parent: Some(self.clone()),
            })),
        }
    }

    pub fn get(&self, name: &str) -> Option<Value> {
        let inner = self.inner.lock().unwrap();
        if let Some(val) = inner.values.get(name) {
            Some(val.clone())
        } else if let Some(ref parent) = inner.parent {
            parent.get(name)
        } else {
            None
        }
    }

    pub fn define(&self, name: &str, value: Value, is_mutable: bool) {
        let mut inner = self.inner.lock().unwrap();
        inner.values.insert(name.to_string(), value);
        if is_mutable {
            inner.mutable.insert(name.to_string());
        }
    }

    /// Collect all variable names visible in this scope (for "did you mean?" suggestions).
    pub fn all_names(&self) -> Vec<String> {
        let inner = self.inner.lock().unwrap();
        let mut names: Vec<String> = inner.values.keys().cloned().collect();
        if let Some(ref parent) = inner.parent {
            names.extend(parent.all_names());
        }
        names.sort();
        names.dedup();
        names
    }

    pub fn assign(&self, name: &str, value: Value) -> Result<(), RuntimeError> {
        let mut inner = self.inner.lock().unwrap();
        if inner.values.contains_key(name) {
            if !inner.mutable.contains(name) {
                return Err(RuntimeError::ImmutableAssignment {
                    name: name.to_string(),
                    span: None,
                });
            }
            inner.values.insert(name.to_string(), value);
            return Ok(());
        }
        if let Some(ref parent) = inner.parent {
            let parent = parent.clone();
            drop(inner);
            parent.assign(name, value)
        } else {
            let suggestion = find_suggestion(name, &self.all_names());
            Err(RuntimeError::UndefinedVariable {
                name: name.to_string(),
                span: None,
                suggestion,
            })
        }
    }
}

/// Compute the Levenshtein edit distance between two strings.
fn levenshtein(a: &str, b: &str) -> usize {
    let a_len = a.chars().count();
    let b_len = b.chars().count();
    if a_len == 0 {
        return b_len;
    }
    if b_len == 0 {
        return a_len;
    }

    let mut prev: Vec<usize> = (0..=b_len).collect();
    let mut curr = vec![0usize; b_len + 1];

    for (i, ca) in a.chars().enumerate() {
        curr[0] = i + 1;
        for (j, cb) in b.chars().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            curr[j + 1] = (prev[j] + cost).min(curr[j] + 1).min(prev[j + 1] + 1);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b_len]
}

/// Find the best "did you mean?" suggestion from known names.
fn find_suggestion(name: &str, known: &[String]) -> Option<String> {
    let mut best: Option<(usize, &str)> = None;
    for candidate in known {
        let dist = levenshtein(name, candidate);
        if dist <= 2 && dist > 0 && (best.is_none() || dist < best.unwrap().0) {
            best = Some((dist, candidate));
        }
    }
    best.map(|(_, s)| s.to_string())
}

impl Default for Environment {
    fn default() -> Self {
        Self::new()
    }
}
