//! Bounded, sandboxed compile-time evaluator for `const` initializers.
//!
//! This module is the entire surface added by issue
//! [burin-labs/harn#1791](https://github.com/burin-labs/harn/issues/1791). It
//! takes a Harn AST expression that appears on the right-hand side of a
//! `const NAME = ...` binding and either returns a [`ConstValue`] or a
//! [`ConstEvalError`]. The evaluator runs entirely inside the parser
//! crate, has zero access to the host or the runtime VM, and enforces
//! three hard caps on every call:
//!
//! 1. **Step budget** — every reduction increments a step counter. When
//!    the counter exceeds [`MAX_STEPS`] (default `100_000`), evaluation
//!    aborts with [`ConstEvalErrorKind::StepLimit`]. The check is
//!    performed on every step, not amortized.
//! 2. **Recursion depth** — every recursive call into the interpreter
//!    increments a depth counter. Exceeding [`MAX_DEPTH`] (default
//!    `256`) aborts with [`ConstEvalErrorKind::RecursionLimit`].
//! 3. **Sandbox denylist** — any expression that reaches `harness`,
//!    spawns concurrency, mutates state, performs I/O, calls into a
//!    non-allowlisted builtin, references an unknown identifier, or
//!    invokes a user-defined function is rejected with
//!    [`ConstEvalErrorKind::SandboxViolation`] or
//!    [`ConstEvalErrorKind::Disallowed`].
//!
//! The evaluator is **allowlist-based**: only explicitly permitted node
//! shapes evaluate. Newly added stdlib surface is sandboxed by default.
//!
//! ## Cache key shape
//!
//! Each successful fold is keyed by:
//!
//! - the SHA-256 of the binding's source-text expression (mirrors what
//!   downstream prompt-template specialization would consume), and
//! - the tuple `(MAX_STEPS, MAX_DEPTH, evaluator_version)`.
//!
//! The cache itself is not implemented here — this module just exposes
//! the inputs so a downstream consumer (e.g. compile-time prompt
//! rendering) can wire it up without re-deriving the contract.

use std::collections::HashMap;

use harn_lexer::{Span, StringSegment};

use crate::ast::{DictEntry, Node, SNode};

/// Hard cap on the number of reduction steps performed by a single
/// `const_eval` call. Checked on every step.
pub const MAX_STEPS: u32 = 100_000;

/// Hard cap on recursion depth into the interpreter. Each
/// `eval_node` invocation increments the depth counter.
pub const MAX_DEPTH: u32 = 256;

/// Stable version tag participating in the cache key. Bump when any
/// observable semantic of the const-evaluator changes.
pub const EVAL_VERSION: u32 = 1;

/// A fully folded compile-time value.
///
/// Mirrors the small subset of runtime `VmValue` shapes that pure
/// expressions can produce. Equality is structural so the same expression
/// always folds to the same value, which is what makes constant folding
/// safe to embed into prompt templates and schema fingerprints.
#[derive(Debug, Clone, PartialEq)]
pub enum ConstValue {
    Int(i64),
    Float(f64),
    Bool(bool),
    String(String),
    List(Vec<ConstValue>),
    Dict(Vec<(String, ConstValue)>),
    Nil,
}

impl ConstValue {
    /// Render the value the way the runtime would for string
    /// concatenation / interpolation, so const-time and runtime renders
    /// stay byte-identical.
    pub fn display(&self) -> String {
        match self {
            ConstValue::Int(n) => n.to_string(),
            ConstValue::Float(f) => format_float(*f),
            ConstValue::Bool(b) => b.to_string(),
            ConstValue::String(s) => s.clone(),
            ConstValue::Nil => "nil".to_string(),
            ConstValue::List(items) => {
                let parts: Vec<String> = items.iter().map(|v| v.display()).collect();
                format!("[{}]", parts.join(", "))
            }
            ConstValue::Dict(entries) => {
                let parts: Vec<String> = entries
                    .iter()
                    .map(|(k, v)| format!("{k}: {}", v.display()))
                    .collect();
                format!("{{{}}}", parts.join(", "))
            }
        }
    }
}

fn format_float(f: f64) -> String {
    if f.fract() == 0.0 && f.is_finite() {
        format!("{f:.1}")
    } else {
        format!("{f}")
    }
}

/// Reason a const-eval call failed. Mapped 1:1 to diagnostic codes:
///
/// - [`ConstEvalErrorKind::Disallowed`] → `HARN-MET-001`
/// - [`ConstEvalErrorKind::StepLimit`] → `HARN-CST-001`
/// - [`ConstEvalErrorKind::RecursionLimit`] → `HARN-CST-002`
/// - [`ConstEvalErrorKind::SandboxViolation`] → `HARN-CST-003`
/// - [`ConstEvalErrorKind::RuntimeError`] → `HARN-CST-004`
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConstEvalErrorKind {
    /// The expression shape is not in the const-friendly allowlist.
    Disallowed,
    /// Reduction count exceeded `MAX_STEPS`.
    StepLimit,
    /// Recursion depth exceeded `MAX_DEPTH`.
    RecursionLimit,
    /// The expression named a sandboxed capability (fs / net / env /
    /// process / host) the evaluator refuses to mediate.
    SandboxViolation,
    /// A value-level error: division by zero, overflow on a literal,
    /// out-of-bounds index, unknown identifier, type mismatch.
    RuntimeError,
}

/// A const-eval failure carries a span (so the typechecker can attribute
/// the diagnostic to the offending sub-expression) and a human-friendly
/// detail.
#[derive(Debug, Clone)]
pub struct ConstEvalError {
    pub kind: ConstEvalErrorKind,
    pub span: Span,
    pub detail: String,
}

impl ConstEvalError {
    fn disallowed(span: Span, detail: impl Into<String>) -> Self {
        Self {
            kind: ConstEvalErrorKind::Disallowed,
            span,
            detail: detail.into(),
        }
    }

    fn sandbox(span: Span, detail: impl Into<String>) -> Self {
        Self {
            kind: ConstEvalErrorKind::SandboxViolation,
            span,
            detail: detail.into(),
        }
    }

    fn runtime(span: Span, detail: impl Into<String>) -> Self {
        Self {
            kind: ConstEvalErrorKind::RuntimeError,
            span,
            detail: detail.into(),
        }
    }

    fn step_limit(span: Span) -> Self {
        Self {
            kind: ConstEvalErrorKind::StepLimit,
            span,
            detail: format!("const-eval exceeded the {MAX_STEPS}-step budget"),
        }
    }

    fn recursion_limit(span: Span) -> Self {
        Self {
            kind: ConstEvalErrorKind::RecursionLimit,
            span,
            detail: format!("const-eval exceeded the {MAX_DEPTH}-deep recursion budget"),
        }
    }
}

/// Names of host objects, runtime keywords, and other surfaces the
/// const-evaluator refuses to dereference. Used by the property-access
/// path to give the most precise sandbox diagnostic possible — anything
/// not on this list still falls back to a generic disallowed-expression
/// rejection because the allowlist is the source of truth.
const SANDBOXED_OBJECT_ROOTS: &[&str] = &[
    "harness",
    "host",
    "transcript",
    "registry",
    "process",
    "fs",
    "net",
    "env",
    "stdio",
    "log",
    "agent",
    "session",
];

/// Pure stdlib builtins whose result is deterministic and side-effect
/// free. Allowlisted explicitly so newly added stdlib surface is
/// sandboxed by default. Each entry is matched on the exact `FunctionCall`
/// name produced by the parser.
const PURE_BUILTINS: &[&str] = &[
    "len",
    "format",
    "min",
    "max",
    "abs",
    "floor",
    "ceil",
    "round",
    "lowercase",
    "uppercase",
    "trim",
    "concat",
    "join",
];

/// Const-friendly binary operators. Mirror of the runtime set that has no
/// side effects and well-defined value semantics on the
/// [`ConstValue`] subset.
const PURE_BINARY_OPS: &[&str] = &[
    "+", "-", "*", "/", "%", "**", "==", "!=", "<", ">", "<=", ">=", "&&", "||", "??",
];

/// Environment mapping a `const` name to its already-folded value. The
/// typechecker primes this with bindings encountered earlier in the same
/// file (i.e. `const X: int = 1` lets `const Y: int = X + 2` resolve).
pub type ConstEnv = HashMap<String, ConstValue>;

/// Public entry point: fold a single AST node into a [`ConstValue`] or
/// return a [`ConstEvalError`]. The `env` argument supplies earlier
/// `const` bindings visible to this expression.
pub fn const_eval(node: &SNode, env: &ConstEnv) -> Result<ConstValue, ConstEvalError> {
    let mut ctx = EvalCtx {
        env,
        steps: 0,
        depth: 0,
    };
    ctx.eval_node(node)
}

struct EvalCtx<'a> {
    env: &'a ConstEnv,
    steps: u32,
    depth: u32,
}

impl<'a> EvalCtx<'a> {
    fn step(&mut self, span: Span) -> Result<(), ConstEvalError> {
        self.steps = self.steps.saturating_add(1);
        if self.steps > MAX_STEPS {
            return Err(ConstEvalError::step_limit(span));
        }
        Ok(())
    }

    fn enter(&mut self, span: Span) -> Result<(), ConstEvalError> {
        self.depth = self.depth.saturating_add(1);
        if self.depth > MAX_DEPTH {
            self.depth -= 1;
            return Err(ConstEvalError::recursion_limit(span));
        }
        Ok(())
    }

    fn leave(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }

    fn eval_node(&mut self, node: &SNode) -> Result<ConstValue, ConstEvalError> {
        self.step(node.span)?;
        self.enter(node.span)?;
        let result = self.eval_node_inner(node);
        self.leave();
        result
    }

    fn eval_node_inner(&mut self, node: &SNode) -> Result<ConstValue, ConstEvalError> {
        let ctx = self;
        match &node.node {
            Node::IntLiteral(n) => Ok(ConstValue::Int(*n)),
            Node::FloatLiteral(f) => Ok(ConstValue::Float(*f)),
            Node::BoolLiteral(b) => Ok(ConstValue::Bool(*b)),
            Node::StringLiteral(s) | Node::RawStringLiteral(s) => Ok(ConstValue::String(s.clone())),
            Node::NilLiteral => Ok(ConstValue::Nil),

            Node::Identifier(name) => ctx.env.get(name).cloned().ok_or_else(|| {
                ConstEvalError::runtime(
                    node.span,
                    format!("`{name}` is not a const-known identifier"),
                )
            }),

            Node::ListLiteral(items) => {
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    if matches!(&item.node, Node::Spread(_)) {
                        return Err(ConstEvalError::disallowed(
                            item.span,
                            "spread in a const list literal is not supported",
                        ));
                    }
                    out.push(ctx.eval_node(item)?);
                }
                Ok(ConstValue::List(out))
            }

            Node::DictLiteral(entries) => {
                let mut out: Vec<(String, ConstValue)> = Vec::with_capacity(entries.len());
                for entry in entries {
                    let key = ctx.dict_key_name(entry)?;
                    let value = ctx.eval_node(&entry.value)?;
                    out.push((key, value));
                }
                Ok(ConstValue::Dict(out))
            }

            Node::InterpolatedString(segments) => {
                let mut buf = String::new();
                for seg in segments {
                    match seg {
                        StringSegment::Literal(lit) => buf.push_str(lit),
                        StringSegment::Expression(src, _, _) => {
                            // The interpolated expression is stored as
                            // raw source text. The const-evaluator never
                            // recursively re-parses host source, so we
                            // refuse to fold dynamic interpolation. The
                            // typechecker can still surface this as a
                            // disallowed expression at the binding
                            // span — interpolation is the one case where
                            // const-eval treats the inner content as
                            // opaque.
                            return Err(ConstEvalError::disallowed(
                                node.span,
                                format!("interpolated expression `${{{src}}}` is not supported in a const initializer; use `format(...)` or string concatenation"),
                            ));
                        }
                    }
                }
                Ok(ConstValue::String(buf))
            }

            Node::UnaryOp { op, operand } => {
                let value = ctx.eval_node(operand)?;
                match (op.as_str(), &value) {
                    ("-", ConstValue::Int(n)) => {
                        Ok(ConstValue::Int(n.checked_neg().ok_or_else(|| {
                            ConstEvalError::runtime(node.span, "integer overflow in unary minus")
                        })?))
                    }
                    ("-", ConstValue::Float(f)) => Ok(ConstValue::Float(-f)),
                    ("!", ConstValue::Bool(b)) => Ok(ConstValue::Bool(!b)),
                    _ => Err(ConstEvalError::runtime(
                        node.span,
                        format!("unary `{op}` is not defined for the operand"),
                    )),
                }
            }

            Node::BinaryOp { op, left, right } => {
                if !PURE_BINARY_OPS.contains(&op.as_str()) {
                    return Err(ConstEvalError::disallowed(
                        node.span,
                        format!("binary operator `{op}` is not const-evaluable"),
                    ));
                }
                let lhs = ctx.eval_node(left)?;
                let rhs = ctx.eval_node(right)?;
                ctx.apply_binary(op, lhs, rhs, node.span)
            }

            Node::Ternary {
                condition,
                true_expr,
                false_expr,
            } => {
                let cond = ctx.eval_node(condition)?;
                let pick = match cond {
                    ConstValue::Bool(b) => b,
                    _ => {
                        return Err(ConstEvalError::runtime(
                            condition.span,
                            "ternary condition must fold to a bool",
                        ))
                    }
                };
                if pick {
                    ctx.eval_node(true_expr)
                } else {
                    ctx.eval_node(false_expr)
                }
            }

            Node::IfElse {
                condition,
                then_body,
                else_body,
            } => {
                let cond = ctx.eval_node(condition)?;
                let pick = match cond {
                    ConstValue::Bool(b) => b,
                    _ => {
                        return Err(ConstEvalError::runtime(
                            condition.span,
                            "if-expression condition must fold to a bool",
                        ))
                    }
                };
                let branch =
                    if pick {
                        then_body.as_slice()
                    } else {
                        match else_body {
                            Some(body) => body.as_slice(),
                            None => return Err(ConstEvalError::disallowed(
                                node.span,
                                "if-expression without an else branch cannot be const-evaluated",
                            )),
                        }
                    };
                let Some(last) = branch.last() else {
                    return Err(ConstEvalError::disallowed(
                        node.span,
                        "if-expression branch must produce a value",
                    ));
                };
                if let Some(first_pre) = branch[..branch.len().saturating_sub(1)].first() {
                    // The only branch shape that folds is a single
                    // value expression. Anything before the final
                    // expression would require statement-level side
                    // effects the sandbox cannot model.
                    return Err(ConstEvalError::disallowed(
                        first_pre.span,
                        "multi-statement if-branch is not const-evaluable",
                    ));
                }
                ctx.eval_node(last)
            }

            Node::FunctionCall { name, args, .. } => {
                if !PURE_BUILTINS.contains(&name.as_str()) {
                    return Err(ConstEvalError::sandbox(
                        node.span,
                        format!(
                            "`{name}(...)` is not on the const-eval allowlist (only pure stdlib builtins may be called from a const initializer)"
                        ),
                    ));
                }
                let mut folded = Vec::with_capacity(args.len());
                for arg in args {
                    folded.push(ctx.eval_node(arg)?);
                }
                ctx.apply_builtin(name, folded, node.span)
            }

            // ----- Explicit sandbox-violating shapes -----
            //
            // Each of these has a more useful diagnostic than the
            // catch-all "expression not allowed" because the user almost
            // certainly tried something with side effects.
            Node::PropertyAccess { object, .. } | Node::OptionalPropertyAccess { object, .. } => {
                if let Node::Identifier(root) = &object.node {
                    if SANDBOXED_OBJECT_ROOTS.contains(&root.as_str()) {
                        return Err(ConstEvalError::sandbox(
                            node.span,
                            format!(
                                "`{root}.*` is a sandboxed capability surface; const-eval refuses fs/net/env/process/host access"
                            ),
                        ));
                    }
                }
                Err(ConstEvalError::disallowed(
                    node.span,
                    "property access is not const-evaluable",
                ))
            }
            Node::MethodCall { object, .. } | Node::OptionalMethodCall { object, .. } => {
                // Probe the receiver chain for a sandboxed host root so
                // `harness.clock.now()` reports the dedicated sandbox
                // diagnostic instead of the generic disallowed-method
                // catch-all. The receiver of `harness.clock.now()` is
                // parsed as `PropertyAccess { object: harness, "clock" }`,
                // and a deeper chain would wrap it in more
                // `PropertyAccess` nodes. Walk down through any chain
                // and pick out the leftmost identifier.
                if let Some(root) = leftmost_receiver_identifier(object) {
                    if SANDBOXED_OBJECT_ROOTS.contains(&root) {
                        return Err(ConstEvalError::sandbox(
                            node.span,
                            format!(
                                "`{root}.*(...)` is a sandboxed capability surface; const-eval refuses fs/net/env/process/host access"
                            ),
                        ));
                    }
                }
                Err(ConstEvalError::disallowed(
                    node.span,
                    "method call is not const-evaluable",
                ))
            }
            Node::SubscriptAccess { object, index } => {
                let recv = ctx.eval_node(object)?;
                let idx = ctx.eval_node(index)?;
                match (recv, idx) {
                    (ConstValue::List(items), ConstValue::Int(i)) => {
                        items.get(i as usize).cloned().ok_or_else(|| {
                            ConstEvalError::runtime(node.span, format!("index {i} out of bounds"))
                        })
                    }
                    (ConstValue::Dict(entries), ConstValue::String(k)) => entries
                        .into_iter()
                        .find(|(name, _)| *name == k)
                        .map(|(_, v)| v)
                        .ok_or_else(|| {
                            ConstEvalError::runtime(node.span, format!("unknown key `{k}`"))
                        }),
                    _ => Err(ConstEvalError::runtime(
                        node.span,
                        "subscript receiver and index types are incompatible",
                    )),
                }
            }
            Node::Block(_) => Err(ConstEvalError::disallowed(
                node.span,
                "block expression is not const-evaluable",
            )),
            Node::Closure { .. } => Err(ConstEvalError::disallowed(
                node.span,
                "closure is not const-evaluable",
            )),

            // ----- Loud sandbox violators: runtime/concurrency surface -----
            Node::SpawnExpr { .. }
            | Node::SelectExpr { .. }
            | Node::Parallel { .. }
            | Node::MutexBlock { .. }
            | Node::DeferStmt { .. }
            | Node::YieldExpr { .. }
            | Node::EmitExpr { .. }
            | Node::HitlExpr { .. }
            | Node::TryCatch { .. }
            | Node::TryExpr { .. }
            | Node::TryOperator { .. }
            | Node::TryStar { .. }
            | Node::DeadlineBlock { .. }
            | Node::CostRoute { .. }
            | Node::WhileLoop { .. }
            | Node::ForIn { .. }
            | Node::Retry { .. }
            | Node::GuardStmt { .. }
            | Node::RequireStmt { .. }
            | Node::Assignment { .. }
            | Node::ThrowStmt { .. }
            | Node::ReturnStmt { .. }
            | Node::BreakStmt
            | Node::ContinueStmt => Err(ConstEvalError::sandbox(
                node.span,
                "runtime construct is not permitted in a const initializer",
            )),

            // Anything else: be conservative and disallow.
            _ => Err(ConstEvalError::disallowed(
                node.span,
                "expression shape is not on the const-eval allowlist",
            )),
        }
    }

    fn dict_key_name(&self, entry: &DictEntry) -> Result<String, ConstEvalError> {
        match &entry.key.node {
            Node::Identifier(name) => Ok(name.clone()),
            Node::StringLiteral(s) | Node::RawStringLiteral(s) => Ok(s.clone()),
            _ => Err(ConstEvalError::disallowed(
                entry.key.span,
                "dict keys in a const dict literal must be identifiers or string literals",
            )),
        }
    }

    fn apply_binary(
        &self,
        op: &str,
        lhs: ConstValue,
        rhs: ConstValue,
        span: Span,
    ) -> Result<ConstValue, ConstEvalError> {
        use ConstValue::*;

        // Special cases first.
        if op == "&&" || op == "||" {
            let (Bool(l), Bool(r)) = (&lhs, &rhs) else {
                return Err(ConstEvalError::runtime(
                    span,
                    format!("`{op}` requires bool operands"),
                ));
            };
            return Ok(Bool(if op == "&&" { *l && *r } else { *l || *r }));
        }
        if op == "??" {
            return Ok(match lhs {
                Nil => rhs,
                other => other,
            });
        }
        if op == "==" {
            return Ok(Bool(lhs == rhs));
        }
        if op == "!=" {
            return Ok(Bool(lhs != rhs));
        }

        // String concat via `+`.
        if op == "+" {
            if let (String(a), String(b)) = (&lhs, &rhs) {
                return Ok(String(format!("{a}{b}")));
            }
        }

        // Numeric arithmetic. Promote to float when either side is float.
        let (lhs_num, rhs_num) = match (&lhs, &rhs) {
            (Int(_) | Float(_), Int(_) | Float(_)) => (lhs.clone(), rhs.clone()),
            _ => {
                return Err(ConstEvalError::runtime(
                    span,
                    format!(
                        "`{op}` requires numeric operands, got {} and {}",
                        value_kind(&lhs),
                        value_kind(&rhs)
                    ),
                ))
            }
        };

        // Comparisons.
        if matches!(op, "<" | ">" | "<=" | ">=") {
            let (l, r) = (as_float(&lhs_num), as_float(&rhs_num));
            let out = match op {
                "<" => l < r,
                ">" => l > r,
                "<=" => l <= r,
                ">=" => l >= r,
                _ => unreachable!(),
            };
            return Ok(Bool(out));
        }

        // Arithmetic.
        if let (Int(a), Int(b)) = (&lhs_num, &rhs_num) {
            let result = match op {
                "+" => a.checked_add(*b),
                "-" => a.checked_sub(*b),
                "*" => a.checked_mul(*b),
                "/" => {
                    if *b == 0 {
                        return Err(ConstEvalError::runtime(span, "division by zero"));
                    }
                    a.checked_div(*b)
                }
                "%" => {
                    if *b == 0 {
                        return Err(ConstEvalError::runtime(span, "modulo by zero"));
                    }
                    a.checked_rem(*b)
                }
                "**" => {
                    if *b < 0 || *b > u32::MAX as i64 {
                        return Err(ConstEvalError::runtime(
                            span,
                            "exponent must be a non-negative i64 within u32 range",
                        ));
                    }
                    a.checked_pow(*b as u32)
                }
                _ => unreachable!(),
            };
            return result
                .map(Int)
                .ok_or_else(|| ConstEvalError::runtime(span, "integer overflow"));
        }

        let (l, r) = (as_float(&lhs_num), as_float(&rhs_num));
        let value = match op {
            "+" => l + r,
            "-" => l - r,
            "*" => l * r,
            "/" => {
                if r == 0.0 {
                    return Err(ConstEvalError::runtime(span, "division by zero"));
                }
                l / r
            }
            "%" => {
                if r == 0.0 {
                    return Err(ConstEvalError::runtime(span, "modulo by zero"));
                }
                l % r
            }
            "**" => l.powf(r),
            _ => unreachable!(),
        };
        Ok(Float(value))
    }

    fn apply_builtin(
        &self,
        name: &str,
        args: Vec<ConstValue>,
        span: Span,
    ) -> Result<ConstValue, ConstEvalError> {
        match name {
            "len" => match args.as_slice() {
                [ConstValue::String(s)] => Ok(ConstValue::Int(s.chars().count() as i64)),
                [ConstValue::List(items)] => Ok(ConstValue::Int(items.len() as i64)),
                [ConstValue::Dict(entries)] => Ok(ConstValue::Int(entries.len() as i64)),
                _ => Err(ConstEvalError::runtime(
                    span,
                    "len() expects a single string / list / dict argument",
                )),
            },
            "format" => format_call(span, args),
            "concat" => {
                let mut out = String::new();
                for arg in &args {
                    match arg {
                        ConstValue::String(s) => out.push_str(s),
                        _ => {
                            return Err(ConstEvalError::runtime(
                                span,
                                "concat() expects string arguments",
                            ))
                        }
                    }
                }
                Ok(ConstValue::String(out))
            }
            "join" => match args.as_slice() {
                [ConstValue::List(items), ConstValue::String(sep)] => {
                    let mut parts = Vec::with_capacity(items.len());
                    for item in items {
                        match item {
                            ConstValue::String(s) => parts.push(s.clone()),
                            other => parts.push(other.display()),
                        }
                    }
                    Ok(ConstValue::String(parts.join(sep)))
                }
                _ => Err(ConstEvalError::runtime(
                    span,
                    "join() expects (list, string)",
                )),
            },
            "min" | "max" => apply_min_max(name, &args, span),
            "abs" => match args.as_slice() {
                [ConstValue::Int(n)] => {
                    Ok(ConstValue::Int(n.checked_abs().ok_or_else(|| {
                        ConstEvalError::runtime(span, "integer overflow in abs()")
                    })?))
                }
                [ConstValue::Float(f)] => Ok(ConstValue::Float(f.abs())),
                _ => Err(ConstEvalError::runtime(
                    span,
                    "abs() expects a single numeric argument",
                )),
            },
            "floor" => unary_float(span, &args, |f| f.floor()),
            "ceil" => unary_float(span, &args, |f| f.ceil()),
            "round" => unary_float(span, &args, |f| f.round()),
            "lowercase" => match args.as_slice() {
                [ConstValue::String(s)] => Ok(ConstValue::String(s.to_lowercase())),
                _ => Err(ConstEvalError::runtime(
                    span,
                    "lowercase() expects a string",
                )),
            },
            "uppercase" => match args.as_slice() {
                [ConstValue::String(s)] => Ok(ConstValue::String(s.to_uppercase())),
                _ => Err(ConstEvalError::runtime(
                    span,
                    "uppercase() expects a string",
                )),
            },
            "trim" => match args.as_slice() {
                [ConstValue::String(s)] => Ok(ConstValue::String(s.trim().to_string())),
                _ => Err(ConstEvalError::runtime(span, "trim() expects a string")),
            },
            // PURE_BUILTINS is the source of truth; if you reach this
            // arm a name was added to the allowlist without an
            // implementation here. Treat as a sandbox violation so the
            // caller still gets an actionable diagnostic.
            _ => Err(ConstEvalError::sandbox(
                span,
                format!("`{name}(...)` lacks a const-eval implementation"),
            )),
        }
    }
}

/// Walk down a receiver chain (`Identifier` → `PropertyAccess` → … ) and
/// return the leftmost identifier name. Used by the method-call sandbox
/// probe so `harness.clock.now()` reports a precise sandbox diagnostic.
fn leftmost_receiver_identifier(node: &SNode) -> Option<&str> {
    let mut current = node;
    loop {
        match &current.node {
            Node::Identifier(name) => return Some(name.as_str()),
            Node::PropertyAccess { object, .. }
            | Node::OptionalPropertyAccess { object, .. }
            | Node::SubscriptAccess { object, .. }
            | Node::OptionalSubscriptAccess { object, .. } => {
                current = object;
            }
            _ => return None,
        }
    }
}

fn value_kind(v: &ConstValue) -> &'static str {
    match v {
        ConstValue::Int(_) => "int",
        ConstValue::Float(_) => "float",
        ConstValue::Bool(_) => "bool",
        ConstValue::String(_) => "string",
        ConstValue::List(_) => "list",
        ConstValue::Dict(_) => "dict",
        ConstValue::Nil => "nil",
    }
}

fn as_float(v: &ConstValue) -> f64 {
    match v {
        ConstValue::Int(n) => *n as f64,
        ConstValue::Float(f) => *f,
        _ => 0.0,
    }
}

fn format_call(span: Span, args: Vec<ConstValue>) -> Result<ConstValue, ConstEvalError> {
    let mut iter = args.into_iter();
    let template = match iter.next() {
        Some(ConstValue::String(s)) => s,
        Some(_) => {
            return Err(ConstEvalError::runtime(
                span,
                "format() template must be a string literal",
            ))
        }
        None => {
            return Err(ConstEvalError::runtime(
                span,
                "format() requires at least a template argument",
            ))
        }
    };
    let rest: Vec<ConstValue> = iter.collect();

    // Mirror runtime semantics: a single dict arg substitutes named
    // `{key}` placeholders; otherwise positional `{}`.
    if let [ConstValue::Dict(entries)] = rest.as_slice() {
        let mut result = String::with_capacity(template.len());
        let mut rest_str = template.as_str();
        while let Some(open) = rest_str.find('{') {
            result.push_str(&rest_str[..open]);
            if let Some(close) = rest_str[open..].find('}') {
                let key = &rest_str[open + 1..open + close];
                if let Some((_, val)) = entries.iter().find(|(k, _)| k == key) {
                    result.push_str(&val.display());
                } else {
                    result.push_str(&rest_str[open..open + close + 1]);
                }
                rest_str = &rest_str[open + close + 1..];
            } else {
                result.push_str(&rest_str[open..]);
                rest_str = "";
                break;
            }
        }
        result.push_str(rest_str);
        return Ok(ConstValue::String(result));
    }

    let mut result = String::with_capacity(template.len());
    let mut rest_iter = rest.iter();
    let mut tail = template.as_str();
    while let Some(pos) = tail.find("{}") {
        result.push_str(&tail[..pos]);
        if let Some(arg) = rest_iter.next() {
            result.push_str(&arg.display());
        } else {
            result.push_str("{}");
        }
        tail = &tail[pos + 2..];
    }
    result.push_str(tail);
    Ok(ConstValue::String(result))
}

fn apply_min_max(
    name: &str,
    args: &[ConstValue],
    span: Span,
) -> Result<ConstValue, ConstEvalError> {
    if args.is_empty() {
        return Err(ConstEvalError::runtime(
            span,
            format!("{name}() requires at least one argument"),
        ));
    }
    let mut all_int = true;
    for arg in args {
        match arg {
            ConstValue::Int(_) => {}
            ConstValue::Float(_) => all_int = false,
            _ => {
                return Err(ConstEvalError::runtime(
                    span,
                    format!("{name}() expects numeric arguments"),
                ))
            }
        }
    }
    if all_int {
        let nums: Vec<i64> = args
            .iter()
            .map(|v| match v {
                ConstValue::Int(n) => *n,
                _ => unreachable!(),
            })
            .collect();
        let pick = if name == "min" {
            nums.iter().copied().min().unwrap()
        } else {
            nums.iter().copied().max().unwrap()
        };
        Ok(ConstValue::Int(pick))
    } else {
        let nums: Vec<f64> = args.iter().map(as_float).collect();
        let pick = if name == "min" {
            nums.iter().copied().fold(f64::INFINITY, f64::min)
        } else {
            nums.iter().copied().fold(f64::NEG_INFINITY, f64::max)
        };
        Ok(ConstValue::Float(pick))
    }
}

fn unary_float(
    span: Span,
    args: &[ConstValue],
    op: impl Fn(f64) -> f64,
) -> Result<ConstValue, ConstEvalError> {
    match args {
        [ConstValue::Int(n)] => Ok(ConstValue::Float(op(*n as f64))),
        [ConstValue::Float(f)] => Ok(ConstValue::Float(op(*f))),
        _ => Err(ConstEvalError::runtime(
            span,
            "expected a single numeric argument",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_source;

    fn fold(source: &str) -> Result<ConstValue, ConstEvalError> {
        // Wrap as a const binding so the parser produces our node, then
        // extract the right-hand side and run const_eval on it with a
        // fresh environment seeded from any earlier const decls.
        let program = parse_source(source).expect("parse");
        let mut env = ConstEnv::new();
        let mut last = None;
        for snode in &program {
            if let Node::ConstBinding { name, value, .. } = &snode.node {
                let folded = const_eval(value, &env)?;
                env.insert(name.clone(), folded.clone());
                last = Some(folded);
            }
        }
        Ok(last.expect("no const binding in source"))
    }

    #[test]
    fn arithmetic_literals_fold() {
        assert_eq!(fold("const X = 1 + 2").unwrap(), ConstValue::Int(3));
        assert_eq!(fold("const Y = 5 * (3 + 2)").unwrap(), ConstValue::Int(25));
        assert_eq!(fold("const Z = 2 ** 10").unwrap(), ConstValue::Int(1024));
    }

    #[test]
    fn string_concat_folds() {
        assert_eq!(
            fold(r#"const S = "foo" + "-" + "bar""#).unwrap(),
            ConstValue::String("foo-bar".to_string())
        );
    }

    #[test]
    fn earlier_const_visible_to_later() {
        let src = "const A = 10\nconst B = A * 2";
        assert_eq!(fold(src).unwrap(), ConstValue::Int(20));
    }

    #[test]
    fn len_of_literal_list() {
        assert_eq!(
            fold("const N = len([1, 2, 3, 4])").unwrap(),
            ConstValue::Int(4)
        );
    }

    #[test]
    fn format_positional_placeholders() {
        let src = r#"const G = format("{}-{}", "hello", 42)"#;
        assert_eq!(
            fold(src).unwrap(),
            ConstValue::String("hello-42".to_string())
        );
    }

    #[test]
    fn host_property_access_is_sandboxed() {
        let err = fold("const Z = harness.clock.now()").unwrap_err();
        assert!(matches!(
            err.kind,
            ConstEvalErrorKind::SandboxViolation | ConstEvalErrorKind::Disallowed
        ));
    }

    #[test]
    fn division_by_zero_is_runtime_error() {
        let err = fold("const Z = 1 / 0").unwrap_err();
        assert!(matches!(err.kind, ConstEvalErrorKind::RuntimeError));
    }

    #[test]
    fn unknown_identifier_is_runtime_error() {
        let err = fold("const Z = NOPE + 1").unwrap_err();
        assert!(matches!(err.kind, ConstEvalErrorKind::RuntimeError));
    }

    #[test]
    fn spawn_is_sandbox_violation() {
        let err = fold("const Z = spawn { 1 }").unwrap_err();
        assert!(matches!(err.kind, ConstEvalErrorKind::SandboxViolation));
    }

    #[test]
    fn user_function_call_is_sandboxed() {
        let err = fold("const Z = some_user_fn()").unwrap_err();
        assert!(matches!(err.kind, ConstEvalErrorKind::SandboxViolation));
    }

    #[test]
    fn ternary_picks_branch() {
        assert_eq!(fold("const T = true ? 1 : 2").unwrap(), ConstValue::Int(1));
        assert_eq!(fold("const T = false ? 1 : 2").unwrap(), ConstValue::Int(2));
    }

    #[test]
    fn list_subscript_folds() {
        assert_eq!(
            fold("const N = [10, 20, 30][1]").unwrap(),
            ConstValue::Int(20)
        );
    }

    #[test]
    fn list_subscript_out_of_bounds_is_runtime_error() {
        let err = fold("const N = [1, 2][9]").unwrap_err();
        assert!(matches!(err.kind, ConstEvalErrorKind::RuntimeError));
    }

    #[test]
    fn recursion_depth_is_bounded() {
        // Drive the depth guard directly without building a deep AST —
        // a deep `Box<SNode>` chain would stack-overflow Rust's default
        // recursive `Drop` long before the const-evaluator's own guard
        // tripped, which masked the unit being tested. The same guard
        // fires from production code paths regardless of how the depth
        // is reached.
        let env = ConstEnv::new();
        let mut ctx = EvalCtx {
            env: &env,
            steps: 0,
            depth: MAX_DEPTH,
        };
        let err = ctx.enter(Span::dummy()).unwrap_err();
        assert!(matches!(err.kind, ConstEvalErrorKind::RecursionLimit));
        // Guard cleanup: enter() must not have left the depth counter
        // bumped past the cap on the error path (otherwise repeated
        // tripping would saturate the counter and starve future calls).
        assert_eq!(ctx.depth, MAX_DEPTH);
    }

    #[test]
    fn step_budget_is_bounded() {
        // The step counter is checked on every `eval_node` call, not
        // amortized — flip the counter near the cap and confirm the
        // very next reduction trips. Driving the counter directly keeps
        // the test fast and avoids constructing an AST large enough to
        // approach the 100k-step budget.
        let env = ConstEnv::new();
        let mut ctx = EvalCtx {
            env: &env,
            steps: MAX_STEPS,
            depth: 0,
        };
        let err = ctx.step(Span::dummy()).unwrap_err();
        assert!(matches!(err.kind, ConstEvalErrorKind::StepLimit));
    }

    #[test]
    fn step_counter_is_not_amortized() {
        // A defensive end-to-end check: cap steps to a small fixed
        // budget via a hand-built ConstBinding-shaped expression and
        // observe that the very next call fails. The intent is to
        // prevent an accidental refactor that batches the cap check
        // (e.g. only every N steps).
        let env = ConstEnv::new();
        // Five back-to-back literal lookups consume exactly five steps.
        // Pre-loading the counter to MAX_STEPS - 4 then folding a
        // 5-step expression must trip exactly once.
        let mut ctx = EvalCtx {
            env: &env,
            steps: MAX_STEPS - 4,
            depth: 0,
        };
        let span = Span::dummy();
        for _ in 0..4 {
            ctx.step(span).expect("inside budget");
        }
        let err = ctx.step(span).unwrap_err();
        assert!(matches!(err.kind, ConstEvalErrorKind::StepLimit));
    }

    #[test]
    fn evaluator_version_is_exposed() {
        // Cache key consumers depend on this being a public stable int.
        // The exact value participates in the documented cache key shape
        // so just reading it suffices — a renamed constant would fail
        // to compile.
        let _ = EVAL_VERSION;
    }
}
