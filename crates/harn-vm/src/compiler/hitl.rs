//! Compiler lowering for first-class HITL primitive expressions.
//!
//! Each [`HitlKind`] is parsed as `Node::HitlExpr` (see
//! `crates/harn-parser/src/parser/expressions.rs::parse_hitl_expr`)
//! and lowered here to a call to the matching async builtin in
//! `crates/harn-vm/src/stdlib/hitl.rs`. The named-argument surface
//! `request_approval(action: ..., quorum: 2, ...)` is canonicalized
//! into the existing positional-plus-options-dict shape that the
//! async builtins consume.
//!
//! The runtime invariants (audit log, quorum-counted distinct
//! reviewers, signature minting, replay determinism) are entirely
//! enforced by the async builtins; this compiler step is only a
//! syntax-to-canonical-shape translation.

use harn_parser::{HitlArg, HitlKind, SNode};

use crate::chunk::Op;

use super::error::CompileError;
use super::Compiler;

impl Compiler {
    pub(super) fn compile_hitl_expr(
        &mut self,
        kind: HitlKind,
        args: &[HitlArg],
    ) -> Result<(), CompileError> {
        match kind {
            HitlKind::AskUser => self.compile_hitl_ask_user(args),
            HitlKind::RequestApproval => self.compile_hitl_request_approval(args),
            HitlKind::DualControl => self.compile_hitl_dual_control(args),
            HitlKind::EscalateTo => self.compile_hitl_escalate_to(args),
        }
    }

    fn compile_hitl_ask_user(&mut self, args: &[HitlArg]) -> Result<(), CompileError> {
        // Canonical builtin shape: `ask_user(prompt, options_dict?)`.
        let prompt = require_arg(args, "ask_user", "prompt", 0)?;
        self.compile_node(prompt)?;

        let option_keys = ["schema", "timeout", "default"];
        // Positional args after prompt are interpreted as the options
        // dict for back-compat with `ask_user("prompt", { ... })`.
        let positional_options = positional_arg_at(args, 1);
        if let Some(options) = positional_options {
            // Reject mixing positional options + named options.
            if args
                .iter()
                .any(|a| a.name.as_deref().is_some_and(|n| option_keys.contains(&n)))
            {
                return Err(CompileError {
                    message:
                        "ask_user: cannot mix positional options dict with named option arguments"
                            .into(),
                    line: self.line,
                });
            }
            self.compile_node(options)?;
            self.emit_named_call("ask_user", 2);
            return Ok(());
        }

        let named_options: Vec<(&str, &SNode)> = option_keys
            .iter()
            .filter_map(|key| {
                args.iter()
                    .find(|a| a.name.as_deref() == Some(key))
                    .map(|a| (*key, &a.value))
            })
            .collect();

        if named_options.is_empty() {
            self.emit_named_call("ask_user", 1);
        } else {
            self.emit_string_keyed_dict(&named_options)?;
            self.emit_named_call("ask_user", 2);
        }
        Ok(())
    }

    fn compile_hitl_request_approval(&mut self, args: &[HitlArg]) -> Result<(), CompileError> {
        // Canonical builtin shape: `request_approval(action, options_dict?)`.
        let action = require_arg(args, "request_approval", "action", 0)?;
        self.compile_node(action)?;

        let option_keys = [
            "args",
            "detail",
            "quorum",
            "reviewers",
            "deadline",
            "principal",
            "evidence_refs",
            "undo_metadata",
            "capabilities_requested",
        ];
        let positional_options = positional_arg_at(args, 1);
        if let Some(options) = positional_options {
            if args
                .iter()
                .any(|a| a.name.as_deref().is_some_and(|n| option_keys.contains(&n)))
            {
                return Err(CompileError {
                    message: "request_approval: cannot mix positional options dict with named option arguments".into(),
                    line: self.line,
                });
            }
            self.compile_node(options)?;
            self.emit_named_call("request_approval", 2);
            return Ok(());
        }

        let named_options: Vec<(&str, &SNode)> = option_keys
            .iter()
            .filter_map(|key| {
                args.iter()
                    .find(|a| a.name.as_deref() == Some(key))
                    .map(|a| (*key, &a.value))
            })
            .collect();
        if named_options.is_empty() {
            self.emit_named_call("request_approval", 1);
        } else {
            self.emit_string_keyed_dict(&named_options)?;
            self.emit_named_call("request_approval", 2);
        }
        Ok(())
    }

    fn compile_hitl_dual_control(&mut self, args: &[HitlArg]) -> Result<(), CompileError> {
        // Canonical builtin shape: `dual_control(n, m, action, approvers?)`.
        let n =
            lookup_arg(args, "n", 0).ok_or_else(|| missing_arg("dual_control", "n", self.line))?;
        let m =
            lookup_arg(args, "m", 1).ok_or_else(|| missing_arg("dual_control", "m", self.line))?;
        let action = lookup_arg(args, "action", 2)
            .ok_or_else(|| missing_arg("dual_control", "action", self.line))?;
        let approvers = lookup_arg(args, "approvers", 3);

        self.compile_node(n)?;
        self.compile_node(m)?;
        self.compile_node(action)?;
        if let Some(approvers) = approvers {
            self.compile_node(approvers)?;
            self.emit_named_call("dual_control", 4);
        } else {
            self.emit_named_call("dual_control", 3);
        }
        Ok(())
    }

    fn compile_hitl_escalate_to(&mut self, args: &[HitlArg]) -> Result<(), CompileError> {
        let role = require_arg(args, "escalate_to", "role", 0)?;
        let reason = require_arg(args, "escalate_to", "reason", 1)?;
        self.compile_node(role)?;
        self.compile_node(reason)?;
        self.emit_named_call("escalate_to", 2);
        Ok(())
    }

    fn emit_string_keyed_dict(&mut self, entries: &[(&str, &SNode)]) -> Result<(), CompileError> {
        for (key, value) in entries {
            // Emit each key as a string constant on the stack, then
            // the value, alternating: BuildDict consumes them as
            // pairs. This mirrors `compile_dict_literal` for the
            // non-spread path.
            let key_idx = self
                .chunk
                .add_constant(crate::chunk::Constant::String((*key).to_string()));
            self.chunk.emit_u16(Op::Constant, key_idx, self.line);
            self.compile_node(value)?;
        }
        self.chunk
            .emit_u16(Op::BuildDict, entries.len() as u16, self.line);
        Ok(())
    }
}

fn lookup_arg<'a>(args: &'a [HitlArg], name: &str, positional_index: usize) -> Option<&'a SNode> {
    if let Some(named) = args.iter().find(|a| a.name.as_deref() == Some(name)) {
        return Some(&named.value);
    }
    positional_arg_at(args, positional_index)
}

fn positional_arg_at(args: &[HitlArg], idx: usize) -> Option<&SNode> {
    args.iter()
        .filter(|a| a.name.is_none())
        .nth(idx)
        .map(|a| &a.value)
}

fn require_arg<'a>(
    args: &'a [HitlArg],
    builtin: &'static str,
    name: &'static str,
    positional_index: usize,
) -> Result<&'a SNode, CompileError> {
    lookup_arg(args, name, positional_index).ok_or_else(|| missing_arg(builtin, name, 0))
}

fn missing_arg(builtin: &'static str, name: &'static str, line: u32) -> CompileError {
    CompileError {
        message: format!("{builtin}: missing required argument `{name}`"),
        line,
    }
}
