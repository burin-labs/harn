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
    let linted = Rc::new(Linted {
        start: source.as_ptr(),
        len: source.len(),
        tokens: OnceCell::new(),
    });
    let previous = LINTED.with(|cell| cell.replace(Some(linted)));
    let result = f();
    LINTED.with(|cell| *cell.borrow_mut() = previous);
    result
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
