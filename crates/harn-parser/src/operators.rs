//! Operator vocabulary: spelling, precedence, and associativity.
//!
//! This module is the single owner of Harn's expression-operator grammar. The
//! parser climbs [`OperatorGrammar::CURRENT`], the formatter and `harn fix`
//! decide parenthesization with [`operand_needs_parens`], and the spec's
//! precedence table and the tree-sitter grammar's precedence constants are
//! generated from or drift-checked against [`Precedence::ALL`].
//!
//! The groups follow Swift's standard precedence groups where Harn has the
//! same operator: `??` binds looser than range formation and arithmetic and
//! tighter than comparison, and range formation (`a..b`, `a..=b`) binds looser
//! than arithmetic and does not associate.

use std::fmt;

use harn_lexer::TokenKind;

/// A binary operator. Spelling, wire name, and precedence live here so no
/// consumer compares operator text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum BinaryOperator {
    /// `|>`
    Pipe,
    /// `||`
    Or,
    /// `&&`
    And,
    /// `==`
    Eq,
    /// `!=`
    Neq,
    /// `<`
    Lt,
    /// `>`
    Gt,
    /// `<=`
    Lte,
    /// `>=`
    Gte,
    /// `in`
    In,
    /// `not in`
    NotIn,
    /// `??`
    NilCoalesce,
    /// `+`
    Add,
    /// `-`
    Sub,
    /// `*`
    Mul,
    /// `/`
    Div,
    /// `%`
    Mod,
    /// `**`
    Pow,
}

impl BinaryOperator {
    pub const ALL: [BinaryOperator; 18] = [
        BinaryOperator::Pipe,
        BinaryOperator::Or,
        BinaryOperator::And,
        BinaryOperator::Eq,
        BinaryOperator::Neq,
        BinaryOperator::Lt,
        BinaryOperator::Gt,
        BinaryOperator::Lte,
        BinaryOperator::Gte,
        BinaryOperator::In,
        BinaryOperator::NotIn,
        BinaryOperator::NilCoalesce,
        BinaryOperator::Add,
        BinaryOperator::Sub,
        BinaryOperator::Mul,
        BinaryOperator::Div,
        BinaryOperator::Mod,
        BinaryOperator::Pow,
    ];

    /// The operator as written in Harn source.
    #[must_use]
    pub const fn symbol(self) -> &'static str {
        match self {
            BinaryOperator::Pipe => "|>",
            BinaryOperator::Or => "||",
            BinaryOperator::And => "&&",
            BinaryOperator::Eq => "==",
            BinaryOperator::Neq => "!=",
            BinaryOperator::Lt => "<",
            BinaryOperator::Gt => ">",
            BinaryOperator::Lte => "<=",
            BinaryOperator::Gte => ">=",
            BinaryOperator::In => "in",
            BinaryOperator::NotIn => "not in",
            BinaryOperator::NilCoalesce => "??",
            BinaryOperator::Add => "+",
            BinaryOperator::Sub => "-",
            BinaryOperator::Mul => "*",
            BinaryOperator::Div => "/",
            BinaryOperator::Mod => "%",
            BinaryOperator::Pow => "**",
        }
    }

    /// The stable identifier used by AST JSON and other wire projections.
    /// It equals [`Self::symbol`] except that `not in` is spelled `not_in`.
    #[must_use]
    pub const fn wire_name(self) -> &'static str {
        match self {
            BinaryOperator::NotIn => "not_in",
            other => other.symbol(),
        }
    }

    /// Resolve a wire name produced by [`Self::wire_name`].
    #[must_use]
    pub fn from_wire_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|op| op.wire_name() == name)
    }

    #[must_use]
    pub const fn precedence(self) -> Precedence {
        match self {
            BinaryOperator::Pipe => Precedence::Pipe,
            BinaryOperator::Or => Precedence::LogicalOr,
            BinaryOperator::And => Precedence::LogicalAnd,
            BinaryOperator::Eq | BinaryOperator::Neq => Precedence::Equality,
            BinaryOperator::Lt
            | BinaryOperator::Gt
            | BinaryOperator::Lte
            | BinaryOperator::Gte
            | BinaryOperator::In
            | BinaryOperator::NotIn => Precedence::Comparison,
            BinaryOperator::NilCoalesce => Precedence::NilCoalescing,
            BinaryOperator::Add | BinaryOperator::Sub => Precedence::Additive,
            BinaryOperator::Mul | BinaryOperator::Div | BinaryOperator::Mod => {
                Precedence::Multiplicative
            }
            BinaryOperator::Pow => Precedence::Exponent,
        }
    }

    /// Whether a line break may precede the operator inside an expression.
    ///
    /// `-` must not, so a line that starts with `-x` remains a new statement;
    /// `in` and `not in` must not, so a `for x in` header cannot be split.
    #[must_use]
    pub const fn continues_across_newline(self) -> bool {
        !matches!(
            self,
            BinaryOperator::Sub | BinaryOperator::In | BinaryOperator::NotIn
        )
    }

    /// The operator a compound assignment (`+=`, `-=`, ...) applies.
    #[must_use]
    pub const fn is_compound_assignable(self) -> bool {
        matches!(
            self,
            BinaryOperator::Add
                | BinaryOperator::Sub
                | BinaryOperator::Mul
                | BinaryOperator::Div
                | BinaryOperator::Mod
        )
    }

    /// Operators whose result is always a boolean.
    #[must_use]
    pub const fn is_comparison(self) -> bool {
        matches!(
            self.precedence(),
            Precedence::Equality | Precedence::Comparison
        )
    }

    #[must_use]
    pub const fn is_logical(self) -> bool {
        matches!(self, BinaryOperator::And | BinaryOperator::Or)
    }

    #[must_use]
    pub const fn is_arithmetic(self) -> bool {
        matches!(
            self,
            BinaryOperator::Add
                | BinaryOperator::Sub
                | BinaryOperator::Mul
                | BinaryOperator::Div
                | BinaryOperator::Mod
                | BinaryOperator::Pow
        )
    }
}

impl fmt::Display for BinaryOperator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.symbol())
    }
}

impl serde::Serialize for BinaryOperator {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.wire_name())
    }
}

/// A prefix operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum UnaryOperator {
    /// `!`
    Not,
    /// `-`
    Neg,
}

impl UnaryOperator {
    pub const ALL: [UnaryOperator; 2] = [UnaryOperator::Not, UnaryOperator::Neg];

    #[must_use]
    pub const fn symbol(self) -> &'static str {
        match self {
            UnaryOperator::Not => "!",
            UnaryOperator::Neg => "-",
        }
    }
}

impl fmt::Display for UnaryOperator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.symbol())
    }
}

impl serde::Serialize for UnaryOperator {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.symbol())
    }
}

/// How operators of equal precedence group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Associativity {
    Left,
    Right,
    /// Chaining requires explicit parentheses.
    None,
}

impl Associativity {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Associativity::Left => "left",
            Associativity::Right => "right",
            Associativity::None => "none",
        }
    }
}

/// Expression precedence groups, loosest first. The derived ordering is the
/// binding order of [`OperatorGrammar::CURRENT`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Precedence {
    /// `a |> f`
    Pipe,
    /// `c ? a : b`
    Ternary,
    LogicalOr,
    LogicalAnd,
    Equality,
    /// `<`, `>`, `<=`, `>=`, `in`, `not in`
    Comparison,
    /// `??`
    NilCoalescing,
    /// `a..b`, `a..=b`
    Range,
    Additive,
    Multiplicative,
    /// `!x`, `-x`
    Prefix,
    /// `a ** b`
    Exponent,
    /// Calls, member access, subscripts, postfix `?`/`!`, and every primary.
    Postfix,
}

impl Precedence {
    pub const ALL: [Precedence; 13] = [
        Precedence::Pipe,
        Precedence::Ternary,
        Precedence::LogicalOr,
        Precedence::LogicalAnd,
        Precedence::Equality,
        Precedence::Comparison,
        Precedence::NilCoalescing,
        Precedence::Range,
        Precedence::Additive,
        Precedence::Multiplicative,
        Precedence::Prefix,
        Precedence::Exponent,
        Precedence::Postfix,
    ];

    /// Stable group name used by the spec table and the tree-sitter `PREC`
    /// constants.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Precedence::Pipe => "pipe",
            Precedence::Ternary => "ternary",
            Precedence::LogicalOr => "logical_or",
            Precedence::LogicalAnd => "logical_and",
            Precedence::Equality => "equality",
            Precedence::Comparison => "comparison",
            Precedence::NilCoalescing => "nil_coalescing",
            Precedence::Range => "range",
            Precedence::Additive => "additive",
            Precedence::Multiplicative => "multiplicative",
            Precedence::Prefix => "prefix",
            Precedence::Exponent => "exponent",
            Precedence::Postfix => "postfix",
        }
    }

    #[must_use]
    pub const fn associativity(self) -> Associativity {
        match self {
            Precedence::Ternary | Precedence::Prefix | Precedence::Exponent => Associativity::Right,
            Precedence::Range => Associativity::None,
            _ => Associativity::Left,
        }
    }

    /// What the group is, for the spec's precedence table.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Precedence::Pipe => "Pipe",
            Precedence::Ternary => "Ternary conditional",
            Precedence::LogicalOr => "Logical OR",
            Precedence::LogicalAnd => "Logical AND",
            Precedence::Equality => "Equality",
            Precedence::Comparison => "Comparison / membership",
            Precedence::NilCoalescing => "Nil coalescing",
            Precedence::Range => "Range formation",
            Precedence::Additive => "Additive",
            Precedence::Multiplicative => "Multiplicative",
            Precedence::Prefix => "Unary prefix",
            Precedence::Exponent => "Exponentiation",
            Precedence::Postfix => "Postfix",
        }
    }

    /// Source spellings of the operators in this group, for documentation.
    #[must_use]
    pub const fn spellings(self) -> &'static [&'static str] {
        match self {
            Precedence::Pipe => &["|>"],
            Precedence::Ternary => &["? :"],
            Precedence::LogicalOr => &["||"],
            Precedence::LogicalAnd => &["&&"],
            Precedence::Equality => &["==", "!="],
            Precedence::Comparison => &["<", ">", "<=", ">=", "in", "not in"],
            Precedence::NilCoalescing => &["??"],
            Precedence::Range => &[RANGE_EXCLUSIVE, RANGE_INCLUSIVE],
            Precedence::Additive => &["+", "-"],
            Precedence::Multiplicative => &["*", "/", "%"],
            Precedence::Prefix => &["!", "-"],
            Precedence::Exponent => &["**"],
            Precedence::Postfix => &[".", "?.", "[]", "?.[]", "[:]", "()", "?", "!"],
        }
    }

    /// One-based binding strength in the current grammar (1 = loosest).
    #[must_use]
    pub fn level(self) -> usize {
        Self::ALL
            .iter()
            .position(|group| *group == self)
            .map_or(0, |index| index + 1)
    }

    /// The precedence an AST expression node occupies in the current grammar.
    #[must_use]
    pub fn of_node(node: &crate::Node) -> Precedence {
        match node {
            crate::Node::BinaryOp { op, .. } => op.precedence(),
            // `try* x` parses its operand with the unary parser, so it binds
            // exactly like a prefix operator.
            crate::Node::UnaryOp { .. } | crate::Node::TryStar { .. } => Precedence::Prefix,
            crate::Node::RangeExpr { .. } => Precedence::Range,
            crate::Node::Ternary { .. } => Precedence::Ternary,
            _ => Precedence::Postfix,
        }
    }
}

/// Exclusive-end range spelling: `0..n` yields `0` through `n - 1`.
pub const RANGE_EXCLUSIVE: &str = "..";
/// Inclusive-end range spelling: `1..=n` yields `1` through `n`.
pub const RANGE_INCLUSIVE: &str = "..=";

/// Which operand of an operator a child expression occupies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperandSide {
    Left,
    Right,
    /// The single operand of a prefix operator.
    Prefix,
    /// The receiver of a call, member access, subscript, or postfix operator.
    Postfix,
    /// The condition of `c ? a : b`.
    TernaryCondition,
    /// Either branch of `c ? a : b`.
    TernaryBranch,
}

/// The loosest precedence an expression may have to appear bare as the
/// `side` operand of an operator in group `parent`.
#[must_use]
pub fn loosest_bare_operand(parent: Precedence, side: OperandSide) -> Precedence {
    match side {
        OperandSide::Prefix => Precedence::Prefix,
        OperandSide::Postfix => Precedence::Postfix,
        OperandSide::TernaryCondition => Precedence::LogicalOr,
        OperandSide::TernaryBranch => Precedence::Ternary,
        // The exponent accepts a prefix operand (`2 ** -1`) while the base
        // binds tighter than a prefix (`-2 ** 2` is `-(2 ** 2)`).
        OperandSide::Right if parent == Precedence::Exponent => Precedence::Prefix,
        OperandSide::Left | OperandSide::Right => {
            let same_group_allowed = matches!(
                (parent.associativity(), side),
                (Associativity::Left, OperandSide::Left)
                    | (Associativity::Right, OperandSide::Right)
            );
            if same_group_allowed {
                parent
            } else {
                parent.tighter().unwrap_or(Precedence::Postfix)
            }
        }
    }
}

/// Whether `child` must be parenthesized to keep its grouping as the `side`
/// operand of `parent` in the current grammar.
#[must_use]
pub fn operand_needs_parens(parent: Precedence, child: Precedence, side: OperandSide) -> bool {
    child < loosest_bare_operand(parent, side)
}

/// Whether `text` is exactly one parenthesized group: `(a || b)` but not
/// `(a) + (b)`. The parser keeps a grouped expression's parentheses in its
/// span, so source sliced from that span may already carry them.
#[must_use]
pub fn is_parenthesized(text: &str) -> bool {
    let Ok(tokens) = harn_lexer::Lexer::new(text).tokenize() else {
        return false;
    };
    let mut significant = tokens
        .iter()
        .filter(|token| !matches!(token.kind, TokenKind::Newline | TokenKind::Eof));
    if !matches!(significant.next(), Some(token) if token.kind == TokenKind::LParen) {
        return false;
    }
    let mut depth = 1usize;
    for token in significant {
        if depth == 0 {
            return false;
        }
        match token.kind {
            TokenKind::LParen => depth += 1,
            TokenKind::RParen => depth -= 1,
            _ => {}
        }
    }
    depth == 0
}

impl Precedence {
    /// The next tighter group, if any.
    #[must_use]
    pub fn tighter(self) -> Option<Precedence> {
        Self::ALL.get(self.level()).copied()
    }
}

/// A precedence ladder the parser climbs. [`Self::CURRENT`] is the language;
/// [`Self::LEGACY`] reproduces the pre-0.11 operator grammar (`??` between
/// additive and multiplicative, `a to b [exclusive]` ranges just above `|>`)
/// and exists only so `harn fix --migrate-operator-syntax` can read old source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OperatorGrammar {
    ladder: &'static [Precedence],
    legacy_ranges: bool,
}

impl OperatorGrammar {
    pub const CURRENT: OperatorGrammar = OperatorGrammar {
        ladder: &Precedence::ALL,
        legacy_ranges: false,
    };

    pub const LEGACY: OperatorGrammar = OperatorGrammar {
        ladder: &[
            Precedence::Pipe,
            Precedence::Range,
            Precedence::Ternary,
            Precedence::LogicalOr,
            Precedence::LogicalAnd,
            Precedence::Equality,
            Precedence::Comparison,
            Precedence::Additive,
            Precedence::NilCoalescing,
            Precedence::Multiplicative,
            Precedence::Prefix,
            Precedence::Exponent,
            Precedence::Postfix,
        ],
        legacy_ranges: true,
    };

    #[must_use]
    pub fn loosest(self) -> Precedence {
        self.ladder[0]
    }

    /// The group one step tighter than `group` on this ladder.
    #[must_use]
    pub fn tighter(self, group: Precedence) -> Precedence {
        self.ladder
            .iter()
            .position(|candidate| *candidate == group)
            .and_then(|index| self.ladder.get(index + 1))
            .copied()
            .unwrap_or(Precedence::Postfix)
    }

    /// Whether this grammar spells ranges `a to b [exclusive]`.
    #[must_use]
    pub fn legacy_ranges(self) -> bool {
        self.legacy_ranges
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_names_round_trip() {
        for op in BinaryOperator::ALL {
            assert_eq!(BinaryOperator::from_wire_name(op.wire_name()), Some(op));
        }
        assert_eq!(BinaryOperator::from_wire_name("not in"), None);
    }

    #[test]
    fn nil_coalescing_sits_between_comparison_and_range() {
        assert!(Precedence::Comparison < Precedence::NilCoalescing);
        assert!(Precedence::NilCoalescing < Precedence::Range);
        assert!(Precedence::Range < Precedence::Additive);
    }

    #[test]
    fn parenthesization_follows_associativity() {
        let add = Precedence::Additive;
        assert!(!operand_needs_parens(add, add, OperandSide::Left));
        assert!(operand_needs_parens(add, add, OperandSide::Right));
        assert!(operand_needs_parens(
            Precedence::Range,
            Precedence::Range,
            OperandSide::Left
        ));
        assert!(!operand_needs_parens(
            Precedence::Exponent,
            Precedence::Prefix,
            OperandSide::Right
        ));
        assert!(operand_needs_parens(
            Precedence::Exponent,
            Precedence::Prefix,
            OperandSide::Left
        ));
        assert!(operand_needs_parens(
            Precedence::Additive,
            Precedence::NilCoalescing,
            OperandSide::Left
        ));
        assert!(!operand_needs_parens(
            Precedence::NilCoalescing,
            Precedence::Additive,
            OperandSide::Right
        ));
    }

    #[test]
    fn legacy_ladder_is_a_permutation_of_the_current_groups() {
        let mut legacy = OperatorGrammar::LEGACY.ladder.to_vec();
        legacy.sort();
        assert_eq!(legacy, Precedence::ALL.to_vec());
    }
}
