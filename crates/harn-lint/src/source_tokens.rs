//! One tokenization, comments included, of the source being linted.
//!
//! Several rules read the token stream (comment placement, redacted string
//! ranges, parenthesis pairing, trailing commas). Each used to lex the whole
//! source again. [`scope`] marks the source `lint_full` is linting, and
//! [`with_comments`] lexes it once on first use and hands every later caller
//! the same tokens. Any other source, such as a test snippet, is lexed on
//! demand as before.

use std::cell::{OnceCell, RefCell};
use std::rc::Rc;

use harn_lexer::{Lexer, Token};

struct Linted {
    start: *const u8,
    len: usize,
    tokens: OnceCell<Option<Rc<[Token]>>>,
}

thread_local! {
    static LINTED: RefCell<Option<Rc<Linted>>> = const { RefCell::new(None) };
}

/// Run `f` with `source` as the source whose tokens are shared.
pub(crate) fn scope<R>(source: &str, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<Rc<Linted>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            LINTED.with(|cell| *cell.borrow_mut() = self.0.take());
        }
    }
    let linted = Rc::new(Linted {
        start: source.as_ptr(),
        len: source.len(),
        tokens: OnceCell::new(),
    });
    let _restore = Restore(LINTED.with(|cell| cell.replace(Some(linted))));
    f()
}

/// `source` tokenized with comments, or `None` when it fails to lex.
pub(crate) fn with_comments(source: &str) -> Option<Rc<[Token]>> {
    let lex = || {
        Lexer::new(source)
            .tokenize_with_comments()
            .ok()
            .map(Rc::from)
    };
    let linted = LINTED.with(|cell| cell.borrow().clone());
    match linted {
        Some(linted) if linted.start == source.as_ptr() && linted.len == source.len() => {
            linted.tokens.get_or_init(lex).clone()
        }
        _ => lex(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unwinding_nested_scope_restores_the_previous_tokens() {
        let source = String::from("outer");
        scope(&source, || {
            let outer = with_comments(&source).unwrap();
            let result = std::panic::catch_unwind(|| {
                scope("inner", || {
                    assert!(!Rc::ptr_eq(&outer, &with_comments("inner").unwrap()));
                    panic!("rule failed");
                });
            });
            assert!(result.is_err());
            assert!(Rc::ptr_eq(&outer, &with_comments(&source).unwrap()));
        });
        LINTED.with(|cell| assert!(cell.borrow().is_none()));
    }
}
