use super::*;

/// Recognized retry fields, shared by the compact `@job(retry: {...})` dict
/// and the standalone `@retry(...)` attribute (documented aliases — keep them
/// in lockstep).
const RETRY_KNOWN_KEYS: &[&str] = &["max", "max_attempts", "backoff", "policy"];
/// Recognized backoff strategies for both retry surfaces (case-insensitive).
const RETRY_BACKOFFS: &[&str] = &["svix", "linear", "exponential"];

impl TypeChecker {
    /// Validate attribute usage and emit warnings for unknown attributes.
    /// Recognized attribute names are the runtime/tooling attributes plus
    /// the durable-persona annotation set: `persona`, `trigger`, `handoff`,
    /// and `budget`. All other names produce a warning so misspellings
    /// surface early without breaking compilation.
    ///
    /// Flow predicate cross-attribute rules (epic #571 / #579):
    /// - A bare `@invariant` (no arguments) is the Flow predicate marker.
    ///   It must be paired with exactly one of `@deterministic`/`@semantic`
    ///   and an `@archivist(...)` provenance block. The handler-IR
    ///   `@invariant("name", ...)` form (positional args) is a separate
    ///   feature validated in `harn_ir` and is left untouched here.
    /// - `@deterministic` and `@semantic` are mutually exclusive.
    /// - `@archivist(...)` and `@retroactive` only make sense on Flow
    ///   predicate functions; we warn if they appear without `@invariant`.
    pub(in crate::typechecker) fn check_attributes(
        &mut self,
        attributes: &[Attribute],
        inner: &SNode,
    ) {
        for attr in attributes {
            match attr.name.as_str() {
                "deprecated" | "test" | "complexity" | "acp_tool" | "acp_skill" | "invariant"
                | "deterministic" | "semantic" | "archivist" | "retroactive" | "persona"
                | "step" | "trigger" | "handoff" | "budget" | "command" | "serial" | "heavy"
                | "scopes" | "policy" | "route" | "stream" | "job" | "schedule" | "queue"
                | "retry" => {}
                other => {
                    self.warning_at(
                        Code::UnknownAttribute,
                        format!("unknown attribute `@{other}`"),
                        attr.span,
                    );
                }
            }
            self.validate_standard_attribute_args(attr);
            // `@test` marks test pipelines discovered by `harn test`.
            if attr.name == "test" && !matches!(inner.node, Node::Pipeline { .. }) {
                self.warning_at(
                    Code::InvalidAttributeTarget,
                    "`@test` only applies to pipeline declarations".to_string(),
                    attr.span,
                );
            }
            if attr.name == "acp_tool" && !matches!(inner.node, Node::FnDecl { .. }) {
                self.warning_at(
                    Code::InvalidAttributeTarget,
                    "`@acp_tool` only applies to function declarations".to_string(),
                    attr.span,
                );
            }
            if attr.name == "acp_skill" && !matches!(inner.node, Node::FnDecl { .. }) {
                self.warning_at(
                    Code::InvalidAttributeTarget,
                    "`@acp_skill` only applies to function declarations".to_string(),
                    attr.span,
                );
            }
            if matches!(
                attr.name.as_str(),
                "persona" | "trigger" | "handoff" | "budget"
            ) && !matches!(
                inner.node,
                Node::FnDecl { .. } | Node::ToolDecl { .. } | Node::Pipeline { .. }
            ) {
                self.warning_at(
                    Code::InvalidAttributeTarget,
                    format!(
                        "`@{}` only applies to function, tool, or pipeline declarations",
                        attr.name
                    ),
                    attr.span,
                );
            }
            if attr.name == "command" && !matches!(inner.node, Node::Pipeline { .. }) {
                self.warning_at(
                    Code::InvalidAttributeTarget,
                    "`@command` only applies to pipeline declarations".to_string(),
                    attr.span,
                );
            }
            if matches!(attr.name.as_str(), "serial" | "heavy")
                && !matches!(inner.node, Node::Pipeline { .. })
            {
                self.warning_at(
                    Code::InvalidAttributeTarget,
                    format!(
                        "`@{}` only applies to pipeline declarations (use on `@test` or `test_*` pipelines)",
                        attr.name
                    ),
                    attr.span,
                );
            }
            if attr.name == "step" && !matches!(inner.node, Node::FnDecl { .. }) {
                self.warning_at(
                    Code::InvalidAttributeTarget,
                    "`@step` only applies to function declarations".to_string(),
                    attr.span,
                );
            }
            if matches!(attr.name.as_str(), "job" | "schedule" | "queue" | "retry")
                && !matches!(inner.node, Node::FnDecl { .. })
            {
                self.warning_at(
                    Code::InvalidAttributeTarget,
                    format!(
                        "`@{}` only applies to function declarations (worker/job entrypoints)",
                        attr.name
                    ),
                    attr.span,
                );
            }
            if matches!(
                attr.name.as_str(),
                "deterministic" | "semantic" | "archivist" | "retroactive"
            ) && !matches!(inner.node, Node::FnDecl { .. })
            {
                self.warning_at(
                    Code::InvalidAttributeTarget,
                    format!("`@{}` only applies to function declarations", attr.name),
                    attr.span,
                );
            }
            if attr.name == "invariant"
                && !matches!(
                    inner.node,
                    Node::FnDecl { .. } | Node::ToolDecl { .. } | Node::Pipeline { .. }
                )
            {
                self.warning_at(
                    Code::InvalidAttributeTarget,
                    "`@invariant` only applies to function, tool, or pipeline declarations"
                        .to_string(),
                    attr.span,
                );
            }
        }

        // Flow predicate companion-attribute rules. These only apply when a
        // bare `@invariant` (no arguments) is present — that's the Flow
        // predicate marker. Handler-IR-style `@invariant("name", ...)` keeps
        // its existing semantics validated by `harn_ir`.
        let flow_invariant = attributes
            .iter()
            .find(|a| a.name == "invariant" && a.args.is_empty());
        let deterministic = attributes.iter().find(|a| a.name == "deterministic");
        let semantic = attributes.iter().find(|a| a.name == "semantic");
        let archivist = attributes.iter().find(|a| a.name == "archivist");
        let retroactive = attributes.iter().find(|a| a.name == "retroactive");

        if let (Some(det), Some(sem)) = (deterministic, semantic) {
            self.warning_at(
                Code::FlowInvariantAttributeInvalid,
                "`@deterministic` and `@semantic` are mutually exclusive; \
                 a Flow predicate is one mode or the other"
                    .to_string(),
                Span::merge(sem.span, det.span),
            );
        }

        if let Some(inv) = flow_invariant {
            if deterministic.is_none() && semantic.is_none() {
                self.warning_at(
                    Code::FlowInvariantAttributeInvalid,
                    "Flow `@invariant` requires exactly one of `@deterministic` \
                     (default) or `@semantic`"
                        .to_string(),
                    inv.span,
                );
            }
            if archivist.is_none() {
                self.warning_at(
                    Code::FlowInvariantAttributeInvalid,
                    "Flow `@invariant` is missing `@archivist(...)` provenance \
                     (evidence, confidence, source_date, coverage_examples)"
                        .to_string(),
                    inv.span,
                );
            }
        } else {
            if let Some(arch) = archivist {
                self.warning_at(
                    Code::FlowInvariantAttributeInvalid,
                    "`@archivist(...)` only applies to Flow predicates marked \
                     with `@invariant`"
                        .to_string(),
                    arch.span,
                );
            }
            if let Some(retro) = retroactive {
                self.warning_at(
                    Code::FlowInvariantAttributeInvalid,
                    "`@retroactive` only applies to Flow predicates marked \
                     with `@invariant`"
                        .to_string(),
                    retro.span,
                );
            }
        }

        if let Some(arch) = archivist {
            self.validate_archivist_args(arch);
        }
    }

    pub(in crate::typechecker) fn validate_standard_attribute_args(&mut self, attr: &Attribute) {
        match attr.name.as_str() {
            "persona" => self.validate_persona_args(attr),
            "step" => self.validate_step_args(attr),
            "trigger" => self.validate_trigger_args(attr),
            "handoff" => self.validate_handoff_args(attr),
            "budget" => self.validate_budget_args(attr),
            "deprecated" => self.validate_deprecated_args(attr),
            "command" => self.validate_command_args(attr),
            "serial" => self.validate_serial_args(attr),
            "heavy" => self.validate_heavy_args(attr),
            "scopes" => self.validate_scopes_args(attr),
            "policy" => self.validate_policy_args(attr),
            "job" => self.validate_job_args(attr),
            "schedule" => self.validate_schedule_args(attr),
            "queue" => self.validate_queue_args(attr),
            "retry" => self.validate_retry_args(attr),
            "test" if !attr.args.is_empty() => {
                self.warning_at(
                    Code::InvalidAttributeArgument,
                    "`@test` does not accept arguments".to_string(),
                    attr.span,
                );
            }
            _ => {}
        }
    }

    /// `@policy(kinds: "operator", matches: "tenant", methods: "doc.read")`
    /// — declarative route auth-policy metadata that composes with `@scopes`.
    /// `kinds` is enforced by `harn serve site`; `matches` and `methods`
    /// catalog runtime guards implemented with `std/harness/policy`.
    /// Empty or non-string values warn; the route still mounts but the bad
    /// argument is ignored.
    pub(super) fn validate_policy_args(&mut self, attr: &Attribute) {
        const KNOWN_KEYS: &[&str] = &["kinds", "matches", "methods"];
        if attr.args.is_empty() {
            self.warning_at(
                Code::InvalidAttributeArgument,
                "`@policy(...)` requires at least one argument, e.g. `@policy(kinds: \"operator\")`"
                    .to_string(),
                attr.span,
            );
            return;
        }
        for arg in &attr.args {
            let Some(name) = self.require_named_arg("@policy", arg) else {
                continue;
            };
            if !KNOWN_KEYS.contains(&name) {
                self.warning_at(
                    Code::InvalidAttributeArgument,
                    format!("unknown `@policy` argument `{name}`; expected one of {KNOWN_KEYS:?}"),
                    arg.span,
                );
                continue;
            }
            let Some(value) = symbol_like_value(&arg.value.node) else {
                self.warning_at(
                    Code::InvalidAttributeArgument,
                    format!("`@policy({name}: ...)` must be a string literal"),
                    arg.span,
                );
                continue;
            };
            if value.split_whitespace().next().is_none() {
                self.warning_at(
                    Code::InvalidAttributeArgument,
                    format!("`@policy({name}: ...)` cannot be empty"),
                    arg.span,
                );
            }
        }
    }

    pub(super) fn validate_command_args(&mut self, attr: &Attribute) {
        const KNOWN_KEYS: &[&str] = &["name", "description", "hint"];
        for arg in &attr.args {
            let Some(name) = self.require_named_arg("@command", arg) else {
                continue;
            };
            if !KNOWN_KEYS.contains(&name) {
                self.warning_at(
                    Code::InvalidAttributeArgument,
                    format!("unknown `@command` argument `{name}`; expected one of {KNOWN_KEYS:?}"),
                    arg.span,
                );
                continue;
            }
            self.expect_symbol_like("@command", name, &arg.value, arg.span);
        }
    }

    pub(super) fn validate_step_args(&mut self, attr: &Attribute) {
        const KNOWN_KEYS: &[&str] = &[
            "name",
            "model",
            "approval",
            "receipt",
            "error_boundary",
            "retry",
            "budget",
        ];
        let mut has_name = false;
        for arg in &attr.args {
            let Some(name) = self.require_named_arg("@step", arg) else {
                continue;
            };
            if !KNOWN_KEYS.contains(&name) {
                self.warning_at(
                    Code::InvalidAttributeArgument,
                    format!("unknown `@step` argument `{name}`; expected one of {KNOWN_KEYS:?}"),
                    arg.span,
                );
                continue;
            }
            match name {
                "name" => {
                    has_name = true;
                    self.expect_symbol_like("@step", name, &arg.value, arg.span);
                }
                "model" => self.expect_symbol_like("@step", name, &arg.value, arg.span),
                "approval" => self.expect_one_of(
                    "@step",
                    name,
                    &arg.value,
                    arg.span,
                    &["required", "optional"],
                ),
                "receipt" => {
                    self.expect_one_of("@step", name, &arg.value, arg.span, &["audit", "none"]);
                }
                "error_boundary" => self.expect_one_of(
                    "@step",
                    name,
                    &arg.value,
                    arg.span,
                    &["fail", "continue", "escalate"],
                ),
                "retry" => self.expect_step_retry_dict(&arg.value, arg.span),
                "budget" => self.expect_step_budget_dict(&arg.value, arg.span),
                _ => {}
            }
        }
        if !has_name {
            self.warning_at(
                Code::InvalidAttributeArgument,
                "`@step(...)` should declare `name: \"...\"` for stable supervision metadata"
                    .to_string(),
                attr.span,
            );
        }
    }

    pub(super) fn expect_step_budget_dict(&mut self, value: &SNode, span: Span) {
        const NUMBER_KEYS: &[&str] = &["max_tokens", "max_usd"];
        let Node::DictLiteral(entries) = &value.node else {
            self.warning_at(
                Code::InvalidAttributeArgument,
                "`@step(budget: ...)` must be a dict such as `{ max_tokens: 1000, max_usd: 0.05 }`"
                    .to_string(),
                span,
            );
            return;
        };
        for entry in entries {
            let Some(field_name) = attr_key_name(&entry.key.node) else {
                self.warning_at(
                    Code::InvalidAttributeArgument,
                    "`@step(budget: ...)` field names must be strings or identifiers".to_string(),
                    entry.key.span,
                );
                continue;
            };
            if !NUMBER_KEYS.contains(&field_name) {
                self.warning_at(Code::InvalidAttributeArgument,
                    format!(
                        "unknown `@step(budget: ...)` field `{field_name}`; expected one of {NUMBER_KEYS:?}"
                    ),
                    entry.key.span,
                );
                continue;
            }
            match (field_name, &entry.value.node) {
                ("max_tokens", Node::IntLiteral(value)) if *value >= 1 => {}
                ("max_tokens", _) => self.warning_at(
                    Code::InvalidAttributeArgument,
                    "`@step(budget: { max_tokens: ... })` must be a positive integer".to_string(),
                    entry.value.span,
                ),
                ("max_usd", Node::IntLiteral(value)) if *value >= 0 => {}
                ("max_usd", Node::FloatLiteral(value)) if value.is_finite() && *value >= 0.0 => {}
                ("max_usd", _) => self.warning_at(
                    Code::InvalidAttributeArgument,
                    "`@step(budget: { max_usd: ... })` must be a non-negative number".to_string(),
                    entry.value.span,
                ),
                _ => {}
            }
        }
    }

    pub(super) fn validate_persona_args(&mut self, attr: &Attribute) {
        const KNOWN_KEYS: &[&str] = &[
            "name",
            "description",
            "triggers",
            "schedules",
            "tools",
            "autonomy",
            "budget",
            "handoffs",
            "context_packs",
            "evals",
            "receipts",
            "model",
            "owner",
            "stages",
        ];
        for arg in &attr.args {
            let Some(name) = self.require_named_arg("@persona", arg) else {
                continue;
            };
            if !KNOWN_KEYS.contains(&name) {
                self.warning_at(
                    Code::InvalidAttributeArgument,
                    format!("unknown `@persona` argument `{name}`; expected one of {KNOWN_KEYS:?}"),
                    arg.span,
                );
                continue;
            }
            match name {
                "triggers" | "schedules" => {
                    self.expect_list_of_trigger_specs("@persona", name, &arg.value, arg.span);
                }
                "tools" | "handoffs" | "context_packs" | "evals" => {
                    self.expect_list_of_symbols("@persona", name, &arg.value, arg.span);
                }
                "budget" => self.expect_budget_dict("@persona", name, &arg.value, arg.span),
                "stages" => self.expect_persona_stages("@persona", &arg.value, arg.span),
                "receipts" => {
                    if !is_symbol_like(&arg.value.node)
                        && !matches!(arg.value.node, Node::BoolLiteral(_))
                    {
                        self.warning_at(
                            Code::InvalidAttributeArgument,
                            "`@persona(receipts: ...)` must be a string/symbol or bool".to_string(),
                            arg.span,
                        );
                    }
                }
                _ => self.expect_symbol_like("@persona", name, &arg.value, arg.span),
            }
        }
    }

    pub(super) fn expect_persona_stages(&mut self, owner: &str, value: &SNode, span: Span) {
        let Node::ListLiteral(entries) = &value.node else {
            self.warning_at(
                Code::InvalidAttributeArgument,
                format!("`{owner}(stages: ...)` must be a list of stage dicts"),
                span,
            );
            return;
        };
        const KNOWN_STAGE_KEYS: &[&str] = &[
            "name",
            "allowed_tools",
            "side_effect_level",
            "max_iterations",
        ];
        for entry in entries {
            let Node::DictLiteral(fields) = &entry.node else {
                self.warning_at(
                    Code::InvalidAttributeArgument,
                    format!("`{owner}(stages: ...)` entries must be dict literals"),
                    entry.span,
                );
                continue;
            };
            let mut saw_name = false;
            for dict_entry in fields {
                let Some(key) = dict_entry_key_str(&dict_entry.key) else {
                    self.warning_at(
                        Code::InvalidAttributeArgument,
                        format!("`{owner}(stages: ...)` stage keys must be identifiers"),
                        dict_entry.key.span,
                    );
                    continue;
                };
                if !KNOWN_STAGE_KEYS.contains(&key.as_str()) {
                    self.warning_at(
                        Code::InvalidAttributeArgument,
                        format!("unknown stage key `{key}`; expected one of {KNOWN_STAGE_KEYS:?}"),
                        dict_entry.key.span,
                    );
                    continue;
                }
                match key.as_str() {
                    "name" | "side_effect_level" => {
                        if !is_symbol_like(&dict_entry.value.node) {
                            self.warning_at(
                                Code::InvalidAttributeArgument,
                                format!("stage `{key}` must be a string"),
                                dict_entry.value.span,
                            );
                        }
                        if key == "name" {
                            saw_name = true;
                        }
                    }
                    "allowed_tools" => self.expect_list_of_symbols(
                        owner,
                        "allowed_tools",
                        &dict_entry.value,
                        dict_entry.value.span,
                    ),
                    "max_iterations" if !matches!(dict_entry.value.node, Node::IntLiteral(n) if n >= 0) =>
                    {
                        self.warning_at(
                            Code::InvalidAttributeArgument,
                            "stage `max_iterations` must be a non-negative integer".to_string(),
                            dict_entry.value.span,
                        );
                    }
                    _ => {}
                }
            }
            if !saw_name {
                self.warning_at(
                    Code::InvalidAttributeArgument,
                    format!("`{owner}(stages: ...)` entry missing required `name`"),
                    entry.span,
                );
            }
        }
    }

    pub(super) fn validate_trigger_args(&mut self, attr: &Attribute) {
        const KNOWN_KEYS: &[&str] = &[
            "id", "provider", "kind", "event", "when", "schedule", "budget",
        ];
        for arg in &attr.args {
            if arg.name.is_none() {
                if !is_trigger_spec(&arg.value.node) {
                    self.warning_at(Code::InvalidAttributeArgument,
                        "`@trigger(...)` positional arguments must be strings, dotted trigger ids, or schedule(...)"
                            .to_string(),
                        arg.span,
                    );
                }
                continue;
            }
            let name = arg.name.as_deref().unwrap();
            if !KNOWN_KEYS.contains(&name) {
                self.warning_at(
                    Code::InvalidAttributeArgument,
                    format!("unknown `@trigger` argument `{name}`; expected one of {KNOWN_KEYS:?}"),
                    arg.span,
                );
                continue;
            }
            match name {
                "schedule" => {
                    if !is_trigger_spec(&arg.value.node) {
                        self.warning_at(
                            Code::InvalidAttributeArgument,
                            "`@trigger(schedule: ...)` must be a string/symbol or schedule(...)"
                                .to_string(),
                            arg.span,
                        );
                    }
                }
                "budget" => self.expect_budget_dict("@trigger", name, &arg.value, arg.span),
                _ => self.expect_symbol_like("@trigger", name, &arg.value, arg.span),
            }
        }
    }

    pub(super) fn validate_handoff_args(&mut self, attr: &Attribute) {
        const KNOWN_KEYS: &[&str] = &["target", "to", "reason", "schema", "artifact"];
        for arg in &attr.args {
            let Some(name) = self.require_named_arg("@handoff", arg) else {
                continue;
            };
            if !KNOWN_KEYS.contains(&name) {
                self.warning_at(
                    Code::InvalidAttributeArgument,
                    format!("unknown `@handoff` argument `{name}`; expected one of {KNOWN_KEYS:?}"),
                    arg.span,
                );
                continue;
            }
            match name {
                "target" | "to" => {
                    if is_symbol_like(&arg.value.node) {
                        continue;
                    }
                    self.expect_list_of_symbols("@handoff", name, &arg.value, arg.span);
                }
                _ => self.expect_symbol_like("@handoff", name, &arg.value, arg.span),
            }
        }
    }

    pub(super) fn validate_budget_args(&mut self, attr: &Attribute) {
        for arg in &attr.args {
            let Some(name) = self.require_named_arg("@budget", arg) else {
                continue;
            };
            self.expect_budget_value("@budget", name, &arg.value, arg.span);
        }
    }

    pub(super) fn validate_serial_args(&mut self, attr: &Attribute) {
        // `@serial` may be bare or take a single `group: "name"` arg.
        const KNOWN_KEYS: &[&str] = &["group"];
        for arg in &attr.args {
            let Some(name) = self.require_named_arg("@serial", arg) else {
                continue;
            };
            if !KNOWN_KEYS.contains(&name) {
                self.warning_at(
                    Code::InvalidAttributeArgument,
                    format!("unknown `@serial` argument `{name}`; expected one of {KNOWN_KEYS:?}"),
                    arg.span,
                );
                continue;
            }
            self.expect_symbol_like("@serial", name, &arg.value, arg.span);
        }
    }

    pub(super) fn validate_heavy_args(&mut self, attr: &Attribute) {
        // `@heavy` requires a positive integer `threads` arg.
        const KNOWN_KEYS: &[&str] = &["threads"];
        let mut has_threads = false;
        for arg in &attr.args {
            let Some(name) = self.require_named_arg("@heavy", arg) else {
                continue;
            };
            if !KNOWN_KEYS.contains(&name) {
                self.warning_at(
                    Code::InvalidAttributeArgument,
                    format!("unknown `@heavy` argument `{name}`; expected one of {KNOWN_KEYS:?}"),
                    arg.span,
                );
                continue;
            }
            if name == "threads" {
                has_threads = true;
                if !matches!(arg.value.node, Node::IntLiteral(n) if n >= 1) {
                    self.warning_at(
                        Code::InvalidAttributeArgument,
                        "`@heavy(threads: ...)` must be a positive integer".to_string(),
                        arg.span,
                    );
                }
            }
        }
        if !has_threads {
            self.warning_at(
                Code::InvalidAttributeArgument,
                "`@heavy(...)` must specify `threads: <positive int>`".to_string(),
                attr.span,
            );
        }
    }

    /// Validate `@scopes("a:b", "c:d", ...)`. Arguments must be string
    /// literals (positional or named — the string value is what counts);
    /// at least one is required, and each value should be a non-empty
    /// `resource:action` shape. The shape is just a lint here so misspelled
    /// scopes surface at typecheck instead of at the first 403.
    pub(super) fn validate_scopes_args(&mut self, attr: &Attribute) {
        if attr.args.is_empty() {
            self.warning_at(
                Code::InvalidAttributeArgument,
                "`@scopes(...)` requires at least one scope literal, e.g. `@scopes(\"personas:read\")`"
                    .to_string(),
                attr.span,
            );
            return;
        }
        for arg in &attr.args {
            let Some(value) = symbol_like_value(&arg.value.node) else {
                self.warning_at(
                    Code::InvalidAttributeArgument,
                    "`@scopes(...)` arguments must be string literals".to_string(),
                    arg.span,
                );
                continue;
            };
            if value.is_empty() {
                self.warning_at(
                    Code::InvalidAttributeArgument,
                    "`@scopes(...)` arguments cannot be empty strings".to_string(),
                    arg.span,
                );
                continue;
            }
            if !value.contains(':') {
                self.warning_at(
                    Code::InvalidAttributeArgument,
                    format!(
                        "`@scopes({value:?})` should be a `resource:action` literal like `\"personas:read\"`"
                    ),
                    arg.span,
                );
            }
        }
    }

    /// Validate one recognized retry field (`max`/`max_attempts`/`backoff`/
    /// `policy`) for either retry surface. The compact `@job(retry: {...})`
    /// dict and the standalone `@retry(...)` attribute are documented as
    /// aliases, so they MUST validate identically — sharing this helper (and
    /// [`RETRY_KNOWN_KEYS`]/[`RETRY_BACKOFFS`]) is what keeps them from
    /// drifting. `key_span` points at the field name (unknown-key warning),
    /// `value_span` at its value (type warnings). `label` is the surface name
    /// woven into messages (`@job(retry: {{ ... }})` or `@retry(...)`).
    fn validate_retry_field(
        &mut self,
        key: &str,
        value: &Node,
        key_span: Span,
        value_span: Span,
        label: &str,
    ) {
        if !RETRY_KNOWN_KEYS.contains(&key) {
            self.warning_at(
                Code::InvalidAttributeArgument,
                format!("unknown `{label}` field `{key}`; expected one of {RETRY_KNOWN_KEYS:?}"),
                key_span,
            );
            return;
        }
        match (key, value) {
            ("max" | "max_attempts", Node::IntLiteral(i)) if *i >= 0 => {}
            ("max" | "max_attempts", _) => self.warning_at(
                Code::InvalidAttributeArgument,
                format!("`{label}` `max` must be a non-negative integer"),
                value_span,
            ),
            ("backoff" | "policy", value) => {
                let ok = symbol_like_value(value)
                    .map(|v| RETRY_BACKOFFS.contains(&v.to_ascii_lowercase().as_str()))
                    .unwrap_or(false);
                if !ok {
                    self.warning_at(
                        Code::InvalidAttributeArgument,
                        format!("`{label}` `backoff` must be one of {RETRY_BACKOFFS:?}"),
                        value_span,
                    );
                }
            }
            _ => {}
        }
    }

    /// Validate `@job("name", retry: { max:, backoff: })`. The positional
    /// name (optional) must be a string; the compact named `retry` dict is
    /// accepted as an alias for the standalone `@retry(...)` job modifier.
    /// Lint-only: malformed forms warn, never block.
    pub(super) fn validate_job_args(&mut self, attr: &Attribute) {
        const KNOWN_KEYS: &[&str] = &["retry"];
        let mut positionals = 0;
        for arg in &attr.args {
            let Some(name) = arg.name.as_deref() else {
                positionals += 1;
                if positionals > 1 {
                    self.warning_at(
                        Code::InvalidAttributeArgument,
                        "`@job(...)` takes at most one positional name".to_string(),
                        arg.span,
                    );
                } else if !is_string_literal(&arg.value.node) {
                    self.warning_at(
                        Code::InvalidAttributeArgument,
                        "`@job(\"name\")` name must be a string literal".to_string(),
                        arg.span,
                    );
                }
                continue;
            };
            if !KNOWN_KEYS.contains(&name) {
                self.warning_at(
                    Code::InvalidAttributeArgument,
                    format!("unknown `@job` argument `{name}`; expected one of {KNOWN_KEYS:?}"),
                    arg.span,
                );
                continue;
            }
            if name == "retry" {
                self.expect_job_retry_dict(&arg.value, arg.span);
            }
        }
    }

    pub(super) fn expect_job_retry_dict(&mut self, value: &SNode, span: Span) {
        let Node::DictLiteral(entries) = &value.node else {
            self.warning_at(
                Code::InvalidAttributeArgument,
                "`@job(retry: ...)` must be a dict such as `{ max: 3, backoff: \"exponential\" }`"
                    .to_string(),
                span,
            );
            return;
        };
        for entry in entries {
            let Some(field_name) = attr_key_name(&entry.key.node) else {
                self.warning_at(
                    Code::InvalidAttributeArgument,
                    "`@job(retry: ...)` field names must be strings or identifiers".to_string(),
                    entry.key.span,
                );
                continue;
            };
            self.validate_retry_field(
                field_name,
                &entry.value.node,
                entry.key.span,
                entry.value.span,
                "@job(retry: { ... })",
            );
        }
    }

    /// Validate `@retry(max: N, backoff: "strategy")`. Documented as an alias
    /// for the compact `@job(retry: {...})` dict, so it shares
    /// [`TypeChecker::validate_retry_field`] to stay in lockstep.
    pub(super) fn validate_retry_args(&mut self, attr: &Attribute) {
        for arg in &attr.args {
            let Some(name) = arg.name.as_deref() else {
                self.warning_at(
                    Code::InvalidAttributeArgument,
                    "`@retry(...)` accepts named arguments such as `max: 3, backoff: \"exponential\"`"
                        .to_string(),
                    arg.span,
                );
                continue;
            };
            self.validate_retry_field(
                name,
                &arg.value.node,
                arg.span,
                arg.value.span,
                "@retry(...)",
            );
        }
    }

    /// Validate `@schedule("cron")` / `@schedule("cron", "timezone")`.
    pub(super) fn validate_schedule_args(&mut self, attr: &Attribute) {
        if attr.args.is_empty() || attr.args.len() > 2 {
            self.warning_at(
                Code::InvalidAttributeArgument,
                "`@schedule(...)` takes a cron expression and an optional timezone".to_string(),
                attr.span,
            );
        }
        for arg in &attr.args {
            if !is_string_literal(&arg.value.node) {
                self.warning_at(
                    Code::InvalidAttributeArgument,
                    "`@schedule(...)` arguments must be string literals".to_string(),
                    arg.span,
                );
            }
        }
    }

    /// Validate `@queue("queue-name")`.
    pub(super) fn validate_queue_args(&mut self, attr: &Attribute) {
        if attr.args.len() != 1 {
            self.warning_at(
                Code::InvalidAttributeArgument,
                "`@queue(\"name\")` takes exactly one string-literal queue name".to_string(),
                attr.span,
            );
            return;
        }
        if !is_string_literal(&attr.args[0].value.node) {
            self.warning_at(
                Code::InvalidAttributeArgument,
                "`@queue(\"name\")` argument must be a string literal".to_string(),
                attr.args[0].span,
            );
        }
    }

    pub(super) fn validate_deprecated_args(&mut self, attr: &Attribute) {
        const KNOWN_KEYS: &[&str] = &["since", "use"];
        for arg in &attr.args {
            let Some(name) = self.require_named_arg("@deprecated", arg) else {
                continue;
            };
            if !KNOWN_KEYS.contains(&name) {
                self.warning_at(
                    Code::InvalidAttributeArgument,
                    format!(
                        "unknown `@deprecated` argument `{name}`; expected one of {KNOWN_KEYS:?}"
                    ),
                    arg.span,
                );
                continue;
            }
            self.expect_symbol_like("@deprecated", name, &arg.value, arg.span);
        }
    }

    pub(super) fn require_named_arg<'a>(
        &mut self,
        attr_name: &str,
        arg: &'a AttributeArg,
    ) -> Option<&'a str> {
        match arg.name.as_deref() {
            Some(name) => Some(name),
            None => {
                self.warning_at(
                    Code::InvalidAttributeArgument,
                    format!("`{attr_name}(...)` arguments must be named"),
                    arg.span,
                );
                None
            }
        }
    }

    pub(super) fn expect_symbol_like(
        &mut self,
        attr_name: &str,
        key: &str,
        value: &SNode,
        span: Span,
    ) {
        if !is_symbol_like(&value.node) {
            self.warning_at(
                Code::InvalidAttributeArgument,
                format!("`{attr_name}({key}: ...)` must be a string or symbol"),
                span,
            );
        }
    }

    pub(super) fn expect_one_of(
        &mut self,
        attr_name: &str,
        key: &str,
        value: &SNode,
        span: Span,
        allowed: &[&str],
    ) {
        let Some(value) = symbol_like_value(&value.node) else {
            self.warning_at(
                Code::InvalidAttributeArgument,
                format!("`{attr_name}({key}: ...)` must be one of {allowed:?}"),
                span,
            );
            return;
        };
        if !allowed.contains(&value) {
            self.warning_at(
                Code::InvalidAttributeArgument,
                format!("`{attr_name}({key}: ...)` must be one of {allowed:?}"),
                span,
            );
        }
    }

    pub(super) fn expect_list_of_symbols(
        &mut self,
        attr_name: &str,
        key: &str,
        value: &SNode,
        span: Span,
    ) {
        let Node::ListLiteral(items) = &value.node else {
            self.warning_at(
                Code::InvalidAttributeArgument,
                format!("`{attr_name}({key}: ...)` must be a list of strings or symbols"),
                span,
            );
            return;
        };
        if items.iter().any(|item| !is_symbol_like(&item.node)) {
            self.warning_at(
                Code::InvalidAttributeArgument,
                format!("`{attr_name}({key}: ...)` must contain only strings or symbols"),
                span,
            );
        }
    }

    pub(super) fn expect_list_of_trigger_specs(
        &mut self,
        attr_name: &str,
        key: &str,
        value: &SNode,
        span: Span,
    ) {
        let Node::ListLiteral(items) = &value.node else {
            self.warning_at(Code::InvalidAttributeArgument,
                format!(
                    "`{attr_name}({key}: ...)` must be a list of strings, dotted trigger ids, or schedule(...)"
                ),
                span,
            );
            return;
        };
        if items.iter().any(|item| !is_trigger_spec(&item.node)) {
            self.warning_at(Code::InvalidAttributeArgument,
                format!(
                    "`{attr_name}({key}: ...)` must contain only strings, dotted trigger ids, or schedule(...)"
                ),
                span,
            );
        }
    }

    pub(super) fn expect_budget_dict(
        &mut self,
        attr_name: &str,
        key: &str,
        value: &SNode,
        span: Span,
    ) {
        let Node::DictLiteral(entries) = &value.node else {
            self.warning_at(
                Code::InvalidAttributeArgument,
                format!("`{attr_name}({key}: ...)` must be a dict of budget fields"),
                span,
            );
            return;
        };
        for entry in entries {
            let Some(field_name) = attr_key_name(&entry.key.node) else {
                self.warning_at(
                    Code::InvalidAttributeArgument,
                    "budget field names must be strings or identifiers".to_string(),
                    entry.key.span,
                );
                continue;
            };
            self.expect_budget_value(attr_name, field_name, &entry.value, entry.value.span);
        }
    }

    pub(super) fn expect_step_retry_dict(&mut self, value: &SNode, span: Span) {
        let Node::DictLiteral(entries) = &value.node else {
            self.warning_at(
                Code::InvalidAttributeArgument,
                "`@step(retry: ...)` must be a dict such as `{ max_attempts: 3 }`".to_string(),
                span,
            );
            return;
        };
        for entry in entries {
            let Some(field_name) = attr_key_name(&entry.key.node) else {
                self.warning_at(
                    Code::InvalidAttributeArgument,
                    "`@step(retry: ...)` field names must be strings or identifiers".to_string(),
                    entry.key.span,
                );
                continue;
            };
            match field_name {
                "max_attempts" => {
                    if !matches!(entry.value.node, Node::IntLiteral(i) if i >= 1) {
                        self.warning_at(
                            Code::InvalidAttributeArgument,
                            "`@step(retry: { max_attempts: ... })` must be a positive integer"
                                .to_string(),
                            entry.value.span,
                        );
                    }
                }
                other => {
                    self.warning_at(
                        Code::InvalidAttributeArgument,
                        format!(
                            "unknown `@step(retry: ...)` field `{other}`; expected `max_attempts`"
                        ),
                        entry.key.span,
                    );
                }
            }
        }
    }

    pub(super) fn expect_budget_value(
        &mut self,
        attr_name: &str,
        key: &str,
        value: &SNode,
        span: Span,
    ) {
        const NUMBER_KEYS: &[&str] = &[
            "daily_usd",
            "hourly_usd",
            "run_usd",
            "max_tokens",
            "frontier_escalations",
            "max_autonomous_decisions_per_hour",
            "max_autonomous_decisions_per_day",
        ];
        const STRING_KEYS: &[&str] = &["on_exhausted", "on_budget_exhausted"];
        if NUMBER_KEYS.contains(&key) {
            if !matches!(value.node, Node::IntLiteral(_) | Node::FloatLiteral(_)) {
                self.warning_at(
                    Code::InvalidAttributeArgument,
                    format!("`{attr_name}({key}: ...)` must be a number"),
                    span,
                );
            }
        } else if STRING_KEYS.contains(&key) {
            self.expect_symbol_like(attr_name, key, value, span);
        } else {
            self.warning_at(Code::InvalidAttributeArgument,
                format!(
                    "unknown `{attr_name}` budget field `{key}`; expected one of {NUMBER_KEYS:?} or {STRING_KEYS:?}"
                ),
                span,
            );
        }
    }

    /// Sanity-check the shape of an `@archivist(...)` block.
    ///
    /// Recognized arguments (all optional individually, but `evidence`
    /// must be present for the block to carry meaningful provenance):
    /// - `evidence`: list of URL strings (the linter only checks that the
    ///   key exists; deep validation lives in the Archivist persona).
    /// - `confidence`: float between 0.0 and 1.0
    /// - `source_date`: string (ISO-8601 date)
    /// - `coverage_examples`: list of strings
    ///
    /// Unknown keys produce a warning so typos surface early.
    pub(in crate::typechecker) fn validate_archivist_args(&mut self, attr: &Attribute) {
        const KNOWN_KEYS: &[&str] = &["evidence", "confidence", "source_date", "coverage_examples"];

        let mut has_evidence = false;
        for arg in &attr.args {
            let Some(name) = arg.name.as_deref() else {
                self.warning_at(
                    Code::InvalidAttributeArgument,
                    "`@archivist(...)` arguments must be named (e.g. \
                     `evidence: [...], confidence: 0.9`)"
                        .to_string(),
                    arg.span,
                );
                continue;
            };
            if !KNOWN_KEYS.contains(&name) {
                self.warning_at(
                    Code::InvalidAttributeArgument,
                    format!(
                        "unknown `@archivist` argument `{name}`; expected one of \
                         {KNOWN_KEYS:?}"
                    ),
                    arg.span,
                );
                continue;
            }
            if name == "evidence" {
                has_evidence = true;
            }
            // Confidence must be a number between 0 and 1 when supplied as a
            // literal. Bare identifiers (e.g. a constant reference) are
            // allowed and validated at runtime.
            if name == "confidence" {
                match &arg.value.node {
                    Node::FloatLiteral(f) if (0.0..=1.0).contains(f) => {}
                    Node::IntLiteral(i) if *i == 0 || *i == 1 => {}
                    Node::Identifier(_) => {}
                    _ => {
                        self.warning_at(
                            Code::InvalidAttributeArgument,
                            "`@archivist(confidence: ...)` must be a float in \
                             [0.0, 1.0]"
                                .to_string(),
                            arg.span,
                        );
                    }
                }
            }
        }

        if !has_evidence {
            self.warning_at(
                Code::InvalidAttributeArgument,
                "`@archivist(...)` should declare `evidence: [...]` so \
                 predicates can be audited"
                    .to_string(),
                attr.span,
            );
        }
    }
}
