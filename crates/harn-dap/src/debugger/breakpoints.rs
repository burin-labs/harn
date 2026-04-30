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
                        let hit_condition = bp
                            .get("hitCondition")
                            .and_then(|c| c.as_str())
                            .map(|s| s.to_string())
                            .filter(|s| !s.is_empty());
                        let log_message = bp
                            .get("logMessage")
                            .and_then(|c| c.as_str())
                            .map(|s| s.to_string())
                            .filter(|s| !s.is_empty());
                        let triggered_by = bp
                            .get("triggeredBy")
                            .and_then(|t| t.as_array())
                            .map(|arr| arr.iter().filter_map(|v| v.as_i64()).collect::<Vec<_>>())
                            .filter(|v: &Vec<i64>| !v.is_empty());
                        self.breakpoints.push(Breakpoint {
                            id,
                            verified: true,
                            line,
                            source: request_path.clone().map(|p| Source {
                                name: None,
                                path: Some(p),
                                source_reference: None,
                            }),
                            condition,
                            hit_condition,
                            log_message,
                            triggered_by,
                        });
                    }
                }
            }
        }
        // Breakpoints replaced for this file — hit counts from the prior
        // set would be attached to now-stale ids, so drop them. Hit
        // counts on breakpoints in other files survive.
        self.bp_hit_counts.clear();

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

    /// Replace the full function-breakpoint list. The DAP spec sends
    /// the complete set on every edit, so we drop the existing list
    /// wholesale, allocate fresh ids, mirror onto the VM, and echo back
    /// verified=true per registered name.
    pub(crate) fn handle_set_function_breakpoints(&mut self, msg: &DapMessage) -> Vec<DapResponse> {
        self.function_breakpoints.clear();
        if let Some(args) = &msg.arguments {
            if let Some(bps) = args.get("breakpoints").and_then(|b| b.as_array()) {
                for bp in bps {
                    if let Some(name) = bp.get("name").and_then(|n| n.as_str()) {
                        let id = self.next_bp_id;
                        self.next_bp_id += 1;
                        let condition = bp
                            .get("condition")
                            .and_then(|c| c.as_str())
                            .map(|s| s.to_string())
                            .filter(|s| !s.is_empty());
                        let hit_condition = bp
                            .get("hitCondition")
                            .and_then(|c| c.as_str())
                            .map(|s| s.to_string())
                            .filter(|s| !s.is_empty());
                        self.function_breakpoints.push(FunctionBreakpoint {
                            name: name.to_string(),
                            condition,
                            hit_condition,
                            id,
                        });
                    }
                }
            }
        }
        // Mirror onto the VM so Vm::push_closure_frame latches the hit
        // on entry to any matching function.
        if let Some(vm) = &mut self.vm {
            vm.set_function_breakpoints(
                self.function_breakpoints
                    .iter()
                    .map(|fb| fb.name.clone())
                    .collect(),
            );
        }
        // Hit counts from the prior set belong to now-stale ids; drop
        // them so a fresh edit starts counting over.
        self.bp_hit_counts.clear();

        let seq = self.next_seq();
        let verified: Vec<_> = self
            .function_breakpoints
            .iter()
            .map(|fb| {
                json!({
                    "id": fb.id,
                    "verified": true,
                    "line": 0,
                })
            })
            .collect();
        vec![DapResponse::success(
            seq,
            msg.seq,
            "setFunctionBreakpoints",
            Some(json!({ "breakpoints": verified })),
        )]
    }

    pub(crate) fn handle_set_exception_breakpoints(
        &mut self,
        msg: &DapMessage,
    ) -> Vec<DapResponse> {
        // Parse both the legacy `filters: [name]` list and the
        // newer per-filter condition form
        // `filterOptions: [{filterId, condition}]` (#111). The two
        // are union-merged into `exception_filters` — the map keys
        // decide whether a raised exception kind stops; the value
        // carries the optional condition expression.
        let args = msg.arguments.as_ref();
        let simple_filters = args
            .and_then(|a| a.get("filters"))
            .and_then(|f| f.as_array())
            .cloned()
            .unwrap_or_default();
        let filter_options = args
            .and_then(|a| a.get("filterOptions"))
            .and_then(|f| f.as_array())
            .cloned()
            .unwrap_or_default();

        let mut new_filters: BTreeMap<String, Option<String>> = BTreeMap::new();
        for filter in &simple_filters {
            if let Some(name) = filter.as_str() {
                new_filters.insert(name.to_string(), None);
            }
        }
        for opt in &filter_options {
            if let Some(name) = opt.get("filterId").and_then(|v| v.as_str()) {
                let cond = opt
                    .get("condition")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .filter(|s| !s.is_empty());
                new_filters.insert(name.to_string(), cond);
            }
        }

        self.exception_filters = new_filters;
        // Legacy single-toggle behavior: "all" still acts as break-on-
        // anything-thrown for Harn exceptions that don't have a typed
        // kind. Any per-kind filter also keeps break_on_exceptions live
        // because the agent-loop throwers can surface through the same
        // Vm::Thrown path today — the filter gate in step_running_vm
        // then narrows to the selected kinds.
        self.break_on_exceptions = !self.exception_filters.is_empty();

        let seq = self.next_seq();
        vec![DapResponse::success(
            seq,
            msg.seq,
            "setExceptionBreakpoints",
            None,
        )]
    }

    /// True if the filter for `kind` is enabled and (if it has a
    /// condition) the condition evaluates truthy. Called from the
    /// exception-path in step_running_vm to gate a stop on the
    /// user's selection.
    pub(crate) fn exception_filter_matches(&mut self, kind: &str) -> bool {
        let Some(cond) = self.exception_filters.get(kind).cloned() else {
            return false;
        };
        match cond {
            None => true,
            Some(expr) => {
                self.ensure_runtime();
                if let Some(vm) = self.vm.as_mut() {
                    let runtime = self.runtime.as_ref().unwrap();
                    runtime
                        .block_on(async {
                            let local = tokio::task::LocalSet::new();
                            local.run_until(vm.evaluate_in_frame(&expr, 0)).await
                        })
                        .map(|v| v.is_truthy())
                        .unwrap_or(true)
                } else {
                    true
                }
            }
        }
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
        // Fresh run → fresh hit counts, so hitCondition and logpoint
        // counters start over for every debug session.
        self.bp_hit_counts.clear();
        // Fresh run → triggered BPs disarm again; the chain has to
        // re-build from scratch each session so a prior run's
        // trigger doesn't unfairly arm this run's dependent BP.
        self.armed_breakpoints.clear();
        self.running = self.vm.is_some();
    }
}

/// Conditional-breakpoint evaluation outcome. Distinguishes "fire"
/// from "skip" so stepping can continue silently, and surfaces
/// expression errors separately so the IDE can render a diagnostic
/// without silently turning the breakpoint into a no-op.
pub(crate) enum BreakpointCondition {
    Fire,
    Skip,
    /// Expression evaluator returned an error; message goes to the
    /// Debug Console so the user can fix the typo.
    Error(String),
}

/// How the debugger should react to a breakpoint hit after all three
/// gating fields (`hitCondition`, `logMessage`, `condition`) are
/// honoured. A logpoint matches VS Code behavior: emit an `output`
/// event and continue without stopping.
pub(crate) enum BreakpointAction {
    /// Stop execution and emit a `stopped` event.
    Stop,
    /// Continue; do not notify the IDE.
    Skip,
    /// Do not stop, but emit the given interpolated log text as a DAP
    /// `output` event with category `console`.
    LogAndContinue(String),
    /// A gating expression errored; surface the diagnostic and skip.
    Diagnostic(String),
}

/// Parse a VS Code-style hit-condition expression.
/// Accepts `N`, `>=N`, `>N`, `%N`, `==N` (and the obvious `=N` alias).
/// Returns `None` on a malformed input so the caller can surface an
/// error instead of silently never firing.
pub(crate) fn hit_condition_matches(expr: &str, hit_count: u64) -> Option<bool> {
    let expr = expr.trim();
    if let Some(rest) = expr.strip_prefix(">=") {
        return rest.trim().parse::<u64>().ok().map(|n| hit_count >= n);
    }
    if let Some(rest) = expr.strip_prefix("<=") {
        return rest.trim().parse::<u64>().ok().map(|n| hit_count <= n);
    }
    if let Some(rest) = expr.strip_prefix("==") {
        return rest.trim().parse::<u64>().ok().map(|n| hit_count == n);
    }
    if let Some(rest) = expr.strip_prefix('>') {
        return rest.trim().parse::<u64>().ok().map(|n| hit_count > n);
    }
    if let Some(rest) = expr.strip_prefix('<') {
        return rest.trim().parse::<u64>().ok().map(|n| hit_count < n);
    }
    if let Some(rest) = expr.strip_prefix('%') {
        let n = rest.trim().parse::<u64>().ok()?;
        if n == 0 {
            return None;
        }
        return Some(hit_count > 0 && hit_count.is_multiple_of(n));
    }
    if let Some(rest) = expr.strip_prefix('=') {
        return rest.trim().parse::<u64>().ok().map(|n| hit_count == n);
    }
    // Bare `N` — fire exactly on the Nth hit.
    expr.parse::<u64>().ok().map(|n| hit_count == n)
}

/// Look up the condition attached to the breakpoint at `line`, if any.
pub(crate) fn condition_for_line(
    bp_conditions: &[(i64, Option<String>)],
    line: i64,
) -> Option<&str> {
    bp_conditions
        .iter()
        .find(|(l, _)| *l == line)
        .and_then(|(_, c)| c.as_deref())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
}

/// Legacy wrapper retained for the conformance suite: evaluate the
/// condition against a pre-captured `variables` map using the minimal
/// `var <op> literal` parser. New code should route through
/// `Debugger::evaluate_condition` so arbitrary expressions work.
#[allow(dead_code)]
pub(crate) fn check_condition_literal(
    condition: &str,
    variables: &BTreeMap<String, VmValue>,
) -> bool {
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

impl Debugger {
    /// Evaluate a breakpoint's condition expression against the live
    /// VM frame using the unified evaluator. Falls back to the legacy
    /// literal matcher only if the VM is mysteriously absent (which
    /// shouldn't happen when a breakpoint fires, but belt-and-braces).
    pub(crate) fn evaluate_condition(
        &mut self,
        bp_conditions: &[(i64, Option<String>)],
        line: i64,
        variables: &BTreeMap<String, VmValue>,
    ) -> BreakpointCondition {
        let Some(condition) = condition_for_line(bp_conditions, line) else {
            return BreakpointCondition::Fire;
        };
        let condition = condition.to_string();
        if self.vm.is_none() {
            return if check_condition_literal(&condition, variables) {
                BreakpointCondition::Fire
            } else {
                BreakpointCondition::Skip
            };
        }
        match self.evaluate_expression_in_vm(&condition) {
            Ok(val) => {
                if val.is_truthy() {
                    BreakpointCondition::Fire
                } else {
                    BreakpointCondition::Skip
                }
            }
            Err(err) => BreakpointCondition::Error(err),
        }
    }

    /// Full breakpoint-hit decision: bumps the hit counter, checks
    /// `hitCondition`, then honours `logMessage` (don't stop) or the
    /// condition expression (stop if truthy). Returns the combined
    /// action so the caller can decide whether to emit a stopped
    /// event, a log line, or neither.
    ///
    /// Evaluated in order: (1) hit count → (2) log message if any →
    /// (3) condition expression. Each stage is independent; a
    /// breakpoint can combine all three.
    pub(crate) fn classify_breakpoint_hit(
        &mut self,
        line: i64,
        variables: &BTreeMap<String, VmValue>,
    ) -> BreakpointAction {
        // Find the matching breakpoint — there may be more than one on
        // a line in theory, but setBreakpoints de-dupes per file so in
        // practice the first match is authoritative.
        let Some(bp) = self.breakpoints.iter().find(|bp| bp.line == line).cloned() else {
            return BreakpointAction::Stop;
        };

        // Stage 0: triggered-breakpoint gate (#102). When this BP has
        // a `triggeredBy` list and none of the listed trigger ids
        // have fired yet, skip without bumping the hit counter. Once
        // a trigger fires, the BP is armed for the rest of the run.
        if let Some(ref triggers) = bp.triggered_by {
            let armed = self.armed_breakpoints.get(&bp.id).copied().unwrap_or(false);
            if !armed {
                // Check whether any trigger has fired since the run
                // started. The armed_breakpoints map tracks both
                // self-armed (after a trigger fires) and fire-history
                // for triggers themselves — a BP's id appears in the
                // map with `true` once it has fired at least once.
                let any_fired = triggers
                    .iter()
                    .any(|tid| self.armed_breakpoints.get(tid).copied().unwrap_or(false));
                if !any_fired {
                    return BreakpointAction::Skip;
                }
                // A trigger has fired — arm this BP and fall through
                // to the rest of the pipeline.
                self.armed_breakpoints.insert(bp.id, true);
            }
        }

        // Record this BP's fire so any BP that lists it in
        // triggeredBy arms on subsequent hits.
        self.armed_breakpoints.insert(bp.id, true);

        // Always bump the counter before gating. Mirrors VS Code: a
        // logpoint counts as a hit for hitCondition purposes even
        // though it won't stop.
        let hits = {
            let entry = self.bp_hit_counts.entry(bp.id).or_insert(0);
            *entry += 1;
            *entry
        };

        // Stage 1: hit-count gate.
        if let Some(ref expr) = bp.hit_condition {
            match hit_condition_matches(expr, hits) {
                Some(false) => return BreakpointAction::Skip,
                None => {
                    return BreakpointAction::Diagnostic(format!(
                        "Breakpoint hit-count expression '{expr}' at line {line} is \
                         malformed (expected N, >=N, >N, <N, <=N, ==N, or %N)"
                    ));
                }
                Some(true) => {}
            }
        }

        // Stage 2: condition gate. Runs even when a log message is
        // present so a conditional logpoint only fires its message
        // when the condition holds.
        let condition_ok =
            match self.evaluate_condition(&self.bp_conditions.clone(), line, variables) {
                BreakpointCondition::Fire => true,
                BreakpointCondition::Skip => false,
                BreakpointCondition::Error(msg) => {
                    return BreakpointAction::Diagnostic(msg);
                }
            };
        if !condition_ok {
            return BreakpointAction::Skip;
        }

        // Stage 3: logpoint rendering. Logpoints do not stop the VM.
        if let Some(template) = bp.log_message {
            return match self.render_logpoint_template(&template) {
                Ok(rendered) => BreakpointAction::LogAndContinue(rendered),
                Err(msg) => BreakpointAction::Diagnostic(msg),
            };
        }

        BreakpointAction::Stop
    }

    /// Expose the hit counter for a breakpoint id, for tests and the
    /// future DAP `breakpointLocations` enhancement.
    #[cfg(test)]
    pub(crate) fn breakpoint_hit_count(&self, id: i64) -> u64 {
        self.bp_hit_counts.get(&id).copied().unwrap_or(0)
    }

    /// Test-only escape hatch so the logpoint renderer unit tests can
    /// poke the private implementation without standing up a full VM.
    #[cfg(test)]
    pub(crate) fn render_logpoint_template_for_tests(
        &mut self,
        template: &str,
    ) -> Result<String, String> {
        self.render_logpoint_template(template)
    }

    /// Expand `{expr}` interpolations in a logpoint template. The
    /// template syntax matches VS Code: `{` opens an expression,
    /// `}` closes it, and `\{` / `\}` escape literal braces. The
    /// expression is evaluated with the unified evaluator so any
    /// Harn syntax works.
    fn render_logpoint_template(&mut self, template: &str) -> Result<String, String> {
        let mut out = String::with_capacity(template.len());
        let mut chars = template.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '\\' if matches!(chars.peek(), Some('{') | Some('}')) => {
                    // Emit the literal escaped brace.
                    out.push(chars.next().unwrap());
                }
                '{' => {
                    let mut expr = String::new();
                    let mut closed = false;
                    for inner in chars.by_ref() {
                        if inner == '}' {
                            closed = true;
                            break;
                        }
                        expr.push(inner);
                    }
                    if !closed {
                        return Err(format!(
                            "Logpoint template missing closing '}}' for expression '{{{expr}'",
                        ));
                    }
                    if expr.trim().is_empty() {
                        out.push('{');
                        out.push('}');
                        continue;
                    }
                    match self.evaluate_expression_in_vm(&expr) {
                        Ok(val) => out.push_str(&val.display()),
                        Err(err) => out.push_str(&format!("<{err}>")),
                    }
                }
                other => out.push(other),
            }
        }
        Ok(out)
    }
}
