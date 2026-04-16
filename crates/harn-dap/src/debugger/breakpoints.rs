use std::collections::BTreeMap;

use harn_vm::VmValue;
use serde_json::json;

use super::state::Debugger;
use crate::protocol::*;

impl Debugger {
    pub(crate) fn handle_set_breakpoints(&mut self, msg: &DapMessage) -> Vec<DapResponse> {
        // Per DAP spec, each setBreakpoints request is the *complete* set
        // of breakpoints for one source file. We must therefore drop any
        // existing breakpoints for that file before re-adding, but
        // *preserve* breakpoints from other files (multi-file pipelines).
        let request_path = msg
            .arguments
            .as_ref()
            .and_then(|a| a.get("source"))
            .and_then(|s| s.get("path"))
            .and_then(|p| p.as_str())
            .map(|s| s.to_string());

        if let Some(ref path) = request_path {
            self.breakpoints
                .retain(|bp| bp.source.as_ref().and_then(|s| s.path.as_ref()) != Some(path));
        } else {
            // Source-less request -- legacy behavior, clear everything.
            self.breakpoints.clear();
        }

        if let Some(args) = &msg.arguments {
            if let Some(bps) = args.get("breakpoints").and_then(|b| b.as_array()) {
                for bp in bps {
                    if let Some(line) = bp.get("line").and_then(|l| l.as_i64()) {
                        let id = self.next_bp_id;
                        self.next_bp_id += 1;
                        let condition = bp
                            .get("condition")
                            .and_then(|c| c.as_str())
                            .map(|s| s.to_string())
                            .filter(|s| !s.is_empty());
                        self.breakpoints.push(Breakpoint {
                            id,
                            verified: true,
                            line,
                            source: request_path.clone().map(|p| Source {
                                name: None,
                                path: Some(p),
                            }),
                            condition,
                        });
                    }
                }
            }
        }

        if let Some(vm) = &mut self.vm {
            // Push per-file breakpoint sets so the VM can match
            // (file, line) precisely instead of treating the lines as
            // global wildcards.
            let mut by_file: BTreeMap<String, Vec<usize>> = BTreeMap::new();
            for bp in &self.breakpoints {
                let key = bp
                    .source
                    .as_ref()
                    .and_then(|s| s.path.clone())
                    .unwrap_or_default();
                by_file.entry(key).or_default().push(bp.line as usize);
            }
            // Clear stale files first by setting empty for every file we
            // know about -- covers the case where the user removed all
            // breakpoints from a file that previously had some.
            let known_keys: Vec<String> = by_file.keys().cloned().collect();
            for key in known_keys.iter() {
                vm.set_breakpoints_for_file(key, by_file[key].clone());
            }
        }

        let seq = self.next_seq();
        vec![DapResponse::success(
            seq,
            msg.seq,
            "setBreakpoints",
            Some(json!({ "breakpoints": self.breakpoints })),
        )]
    }

    pub(crate) fn handle_set_exception_breakpoints(
        &mut self,
        msg: &DapMessage,
    ) -> Vec<DapResponse> {
        self.break_on_exceptions = msg
            .arguments
            .as_ref()
            .and_then(|a| a.get("filters"))
            .and_then(|f| f.as_array())
            .map(|filters| filters.iter().any(|f| f.as_str() == Some("all")))
            .unwrap_or(false);

        let seq = self.next_seq();
        vec![DapResponse::success(
            seq,
            msg.seq,
            "setExceptionBreakpoints",
            None,
        )]
    }

    /// Transition idle -> running. Snapshots breakpoint conditions and
    /// resets per-run state. Caller sets the appropriate `step_mode` and
    /// VM step flag separately. Returns nothing -- actual stepping happens
    /// later when `main` polls `step_running_vm` between message drains.
    pub(crate) fn enter_running(&mut self) {
        self.bp_conditions = self
            .breakpoints
            .iter()
            .map(|bp| (bp.line, bp.condition.clone()))
            .collect();
        self.var_refs.clear();
        self.next_var_ref = 100;
        self.running = self.vm.is_some();
    }
}

/// Check if a conditional breakpoint should fire.
pub(crate) fn check_condition(
    bp_conditions: &[(i64, Option<String>)],
    line: i64,
    variables: &BTreeMap<String, VmValue>,
) -> bool {
    let condition = bp_conditions
        .iter()
        .find(|(l, _)| *l == line)
        .and_then(|(_, c)| c.as_deref());

    let condition = match condition {
        Some(c) => c.trim(),
        None => return true,
    };

    // Minimal evaluator: `var <op> val` for comparison ops, or bare `var` for truthy.
    for op in &["==", "!=", ">=", "<=", ">", "<"] {
        if let Some((lhs, rhs)) = condition.split_once(op) {
            let lhs = lhs.trim();
            let rhs = rhs.trim().trim_matches('"');
            let lhs_val = variables.get(lhs).map(|v| v.display()).unwrap_or_default();
            return match *op {
                "==" => lhs_val == rhs,
                "!=" => lhs_val != rhs,
                ">=" => lhs_val.parse::<f64>().unwrap_or(0.0) >= rhs.parse::<f64>().unwrap_or(0.0),
                "<=" => lhs_val.parse::<f64>().unwrap_or(0.0) <= rhs.parse::<f64>().unwrap_or(0.0),
                ">" => lhs_val.parse::<f64>().unwrap_or(0.0) > rhs.parse::<f64>().unwrap_or(0.0),
                "<" => lhs_val.parse::<f64>().unwrap_or(0.0) < rhs.parse::<f64>().unwrap_or(0.0),
                _ => true,
            };
        }
    }

    if let Some(val) = variables.get(condition) {
        return val.is_truthy();
    }

    true
}
