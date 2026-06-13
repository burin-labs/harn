//! Skill registry builtins.
//!
//! Skills bundle metadata + tool references + MCP server lists +
//! system-prompt fragments + auto-activation rules into a typed unit
//! that can be registered, enumerated, and selected at runtime. The
//! top-level `skill NAME { ... }` language form (see
//! `crates/harn-parser/src/parser/decls.rs`) and the `@acp_skill`
//! attribute both lower to `skill_define(skill_registry(), name, { ... })`.
//!
//! The shape of each stored skill entry is:
//!
//! ```text
//! {
//!   name: string,                  // required
//!   description: string,           // optional, copied from `description` field if present
//!   // all other user-provided keys (e.g. when_to_use, paths, tools, mcp,
//!   // prompt, on_activate, on_deactivate, invocation, model, effort)
//!   // pass through unchanged.
//! }
//! ```
//!
//! Registries mirror the tool-registry shape: `{ _type: "skill_registry", skills: [ ... ] }`.

use crate::value::VmDictExt;
use std::collections::BTreeMap;

use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

fn vm_validate_registry(name: &str, dict: &crate::value::DictMap) -> Result<(), VmError> {
    match dict.get("_type") {
        Some(VmValue::String(t)) if &**t == "skill_registry" => Ok(()),
        _ => Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
            format!("{name}: argument must be a skill registry (created with skill_registry())"),
        )))),
    }
}

fn vm_get_skills(dict: &crate::value::DictMap) -> &[VmValue] {
    match dict.get("skills") {
        Some(VmValue::List(list)) => list,
        _ => &[],
    }
}

fn vm_skill_entry_id(entry: &crate::value::DictMap) -> String {
    let name = entry.get("name").map(|v| v.display()).unwrap_or_default();
    let namespace = entry
        .get("namespace")
        .map(|v| v.display())
        .filter(|value| !value.is_empty());
    match namespace {
        Some(ns) => format!("{ns}/{name}"),
        None => name,
    }
}

fn vm_skill_catalog_entries(skills: &[VmValue]) -> Vec<VmValue> {
    let mut catalog: Vec<(String, VmValue)> = Vec::new();
    for skill in skills {
        let Some(entry) = skill.as_dict() else {
            continue;
        };
        let id = vm_skill_entry_id(entry);
        if id.is_empty() {
            continue;
        }
        let description = entry
            .get("short")
            .map(|v| v.display())
            .filter(|value| !value.is_empty())
            .or_else(|| {
                entry
                    .get("description")
                    .map(|v| v.display())
                    .filter(|value| !value.is_empty())
            })
            .unwrap_or_default();
        let when_to_use = entry
            .get("when_to_use")
            .map(|v| v.display())
            .unwrap_or_default();
        let mut rendered = BTreeMap::new();
        rendered.put_str("name", id.as_str());
        rendered.put_str("description", description.as_str());
        rendered.put_str("when_to_use", when_to_use.as_str());
        catalog.push((id, VmValue::dict(rendered)));
    }
    catalog.sort_by(|a, b| a.0.cmp(&b.0));
    catalog.into_iter().map(|(_, value)| value).collect()
}

fn vm_skill_who_signed(skills: &[VmValue], target: &str) -> Result<VmValue, VmError> {
    let mut bare_matches: Vec<&crate::value::DictMap> = Vec::new();
    for skill in skills {
        let Some(entry) = skill.as_dict() else {
            continue;
        };
        if vm_skill_entry_id(entry) == target {
            return Ok(who_signed_entry(entry));
        }
        if entry
            .get("name")
            .map(|value| value.display())
            .is_some_and(|name| name == target)
        {
            bare_matches.push(entry);
        }
    }
    match bare_matches.as_slice() {
        [entry] => Ok(who_signed_entry(entry)),
        [] => Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(format!(
            "skill_who_signed: skill '{target}' not found"
        ))))),
        _ => Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(format!(
            "skill_who_signed: skill '{target}' is ambiguous; use the fully qualified id from the catalog"
        ))))),
    }
}

fn who_signed_entry(entry: &crate::value::DictMap) -> VmValue {
    let mut out = match entry.get("provenance").and_then(VmValue::as_dict) {
        Some(provenance) => provenance.clone(),
        None => crate::value::DictMap::new(),
    };
    out.put_str("skill_id", vm_skill_entry_id(entry).as_str());
    out.entry("signed".to_string())
        .or_insert(VmValue::Bool(false));
    out.entry("trusted".to_string())
        .or_insert(VmValue::Bool(false));
    out.entry("endorsements".to_string())
        .or_insert_with(|| VmValue::List(std::sync::Arc::new(Vec::new())));
    VmValue::dict(out)
}

fn render_catalog_entry(entry: &crate::value::DictMap) -> Option<String> {
    let name = entry.get("name").map(|v| v.display()).unwrap_or_default();
    if name.is_empty() {
        return None;
    }
    let description = entry
        .get("short")
        .map(|v| v.display())
        .filter(|value| !value.is_empty())
        .or_else(|| {
            entry
                .get("description")
                .map(|v| v.display())
                .filter(|value| !value.is_empty())
        })
        .unwrap_or_default();
    let when_to_use = entry
        .get("when_to_use")
        .map(|v| v.display())
        .unwrap_or_default();
    let mut lines = Vec::new();
    if description.is_empty() {
        lines.push(format!("- `{name}`"));
    } else {
        lines.push(format!("- `{name}`: {description}"));
    }
    if !when_to_use.is_empty() {
        lines.push(format!("  when: {when_to_use}"));
    }
    Some(lines.join("\n"))
}

fn render_catalog(entries: &[VmValue], budget: usize) -> String {
    let header = concat!(
        "## Available skills\n\n",
        "These skills are available. Call `load_skill({ name: \"<skill-id>\" })` to load the full body of a skill when it becomes relevant.\n\n",
    );
    if entries.is_empty() {
        return format!("{header}(none)");
    }

    let omission_template = "\n\n... 1 more skill(s) omitted to stay within budget.";
    let budget = budget.max(header.len() + omission_template.len());
    let mut blocks = Vec::new();

    for entry in entries {
        let Some(dict) = entry.as_dict() else {
            continue;
        };
        let Some(block) = render_catalog_entry(dict) else {
            continue;
        };
        blocks.push(block);
    }
    if blocks.is_empty() {
        return format!("{header}(none)");
    }

    let mut visible = 0usize;
    let mut rendered = String::from(header);
    while visible < blocks.len() {
        let candidate_len = rendered.len()
            + if visible == 0 {
                blocks[visible].len()
            } else {
                1 + blocks[visible].len()
            };
        if candidate_len > budget {
            break;
        }
        if visible > 0 {
            rendered.push('\n');
        }
        rendered.push_str(&blocks[visible]);
        visible += 1;
    }

    let mut omitted = blocks.len().saturating_sub(visible);
    if omitted > 0 {
        loop {
            let suffix = format!("\n\n... {omitted} more skill(s) omitted to stay within budget.");
            if rendered.len() + suffix.len() <= budget {
                rendered.push_str(&suffix);
                break;
            }
            if visible == 0 {
                break;
            }
            visible -= 1;
            omitted += 1;
            rendered = String::from(header);
            for (index, block) in blocks.iter().take(visible).enumerate() {
                if index > 0 {
                    rendered.push('\n');
                }
                rendered.push_str(block);
            }
        }
    }

    rendered
}

pub(crate) fn register_skill_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

#[harn_builtin(sig = "skill_registry() -> dict", category = "skills")]
fn skill_registry_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let mut registry = BTreeMap::new();
    registry.put_str("_type", "skill_registry");
    registry.insert(
        "skills".to_string(),
        VmValue::List(std::sync::Arc::new(Vec::new())),
    );
    Ok(VmValue::dict(registry))
}

#[harn_builtin(
    sig = "skill_define(registry: dict, name: string, config: dict) -> dict",
    category = "skills"
)]
fn skill_define_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    if args.len() < 3 {
        return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
            "skill_define: requires registry, name, and config dict",
        ))));
    }
    let registry = match &args[0] {
        VmValue::Dict(map) => (**map).clone(),
        _ => {
            return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                "skill_define: first argument must be a skill registry",
            ))));
        }
    };
    vm_validate_registry("skill_define", &registry)?;

    let name = match &args[1] {
        VmValue::String(s) => s.to_string(),
        other => other.display(),
    };
    if name.is_empty() {
        return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
            "skill_define: skill name must be a non-empty string",
        ))));
    }

    let config = match &args[2] {
        VmValue::Dict(map) => (**map).clone(),
        VmValue::Nil => crate::value::DictMap::new(),
        _ => {
            return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                "skill_define: third argument must be a config dict",
            ))));
        }
    };

    // Light validation on known string keys: enforce stable error
    // messages when a user mis-types a value.
    for key in [
        "short",
        "description",
        "when_to_use",
        "prompt",
        "invocation",
        "model",
        "effort",
    ] {
        if let Some(value) = config.get(key) {
            if !matches!(value, VmValue::String(_) | VmValue::Nil) {
                return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                    format!("skill_define: '{key}' must be a string"),
                ))));
            }
        }
    }
    for key in ["paths", "allowed_tools", "mcp", "requires_mcp"] {
        if let Some(value) = config.get(key) {
            if !matches!(value, VmValue::List(_) | VmValue::Nil) {
                return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                    format!("skill_define: '{key}' must be a list"),
                ))));
            }
        }
    }
    // `allowed_tools` supports three shapes per entry:
    //   - exact tool name (no colon)
    //   - `"namespace:<tag>"` (prefix with non-empty tag)
    //   - `"*"` wildcard
    // Anything else with a colon is a typo — fail loud so authors
    // don't silently scope to an empty set.
    if let Some(VmValue::List(list)) = config.get("allowed_tools") {
        for entry in list.iter() {
            let rendered = entry.display();
            if let Some(tag) = rendered.strip_prefix("namespace:") {
                if tag.is_empty() {
                    return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                        "skill_define: 'allowed_tools' entry 'namespace:' missing a tag after the colon",
                    ))));
                }
            } else if rendered.contains(':') {
                return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(format!(
                    "skill_define: 'allowed_tools' entry '{rendered}' contains ':' — only the `namespace:<tag>` prefix is recognized"
                )))));
            }
        }
    }

    let mut entry = BTreeMap::new();
    entry.put_str("name", name.as_str());
    // Keep `description` at the top level even if missing (empty string)
    // so `skill_describe` / transcript surfaces have a stable shape.
    if !config.contains_key("description") {
        let fallback = config
            .get("short")
            .map(|value| value.display())
            .unwrap_or_default();
        entry.put_str("description", "");
        if !fallback.is_empty() {
            entry.put_str("description", fallback.as_str());
        }
    }
    for (k, v) in config.iter() {
        entry.insert(k.clone(), v.clone());
    }
    let entry_value = VmValue::dict(entry);

    let skills = vm_get_skills(&registry);
    let mut new_skills: Vec<VmValue> = Vec::with_capacity(skills.len() + 1);
    let mut replaced = false;
    for existing in skills {
        if let VmValue::Dict(dict) = existing {
            if let Some(VmValue::String(existing_name)) = dict.get("name") {
                if &**existing_name == name.as_str() {
                    new_skills.push(entry_value.clone());
                    replaced = true;
                    continue;
                }
            }
        }
        new_skills.push(existing.clone());
    }
    if !replaced {
        new_skills.push(entry_value);
    }

    let mut new_registry = registry;
    new_registry.insert(
        "skills".to_string(),
        VmValue::List(std::sync::Arc::new(new_skills)),
    );
    Ok(VmValue::dict(new_registry))
}

#[harn_builtin(sig = "skill_list(registry: dict) -> list", category = "skills")]
fn skill_list_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let registry = match args.first() {
        Some(VmValue::Dict(map)) => map,
        _ => {
            return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                "skill_list: requires a skill registry",
            ))));
        }
    };
    vm_validate_registry("skill_list", registry)?;

    let skills = vm_get_skills(registry);
    let mut result = Vec::new();
    for skill in skills {
        if let VmValue::Dict(entry) = skill {
            let mut desc = BTreeMap::new();
            for (key, value) in entry.iter() {
                // Closures (lifecycle hooks) are not JSON-serializable;
                // strip them from the public list like tools strip handlers.
                if matches!(value, VmValue::Closure(_) | VmValue::BuiltinRef(_)) {
                    continue;
                }
                desc.insert(key.clone(), value.clone());
            }
            result.push(VmValue::dict(desc));
        }
    }
    Ok(VmValue::List(std::sync::Arc::new(result)))
}

#[harn_builtin(
    sig = "skills_catalog_entries(registry: dict) -> list",
    category = "skills"
)]
fn skills_catalog_entries_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let registry = match args.first() {
        Some(VmValue::Dict(map)) => map,
        _ => {
            return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                "skills_catalog_entries: requires a skill registry",
            ))));
        }
    };
    vm_validate_registry("skills_catalog_entries", registry)?;
    Ok(VmValue::List(std::sync::Arc::new(
        vm_skill_catalog_entries(vm_get_skills(registry)),
    )))
}

#[harn_builtin(
    sig = "skill_who_signed(registry: dict, name: string) -> dict",
    category = "skills"
)]
fn skill_who_signed_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    if args.len() < 2 {
        return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
            "skill_who_signed: requires registry and skill name",
        ))));
    }
    let registry = match args.first() {
        Some(VmValue::Dict(map)) => map,
        _ => {
            return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                "skill_who_signed: first argument must be a skill registry",
            ))));
        }
    };
    vm_validate_registry("skill_who_signed", registry)?;
    vm_skill_who_signed(vm_get_skills(registry), &args[1].display())
}

#[harn_builtin(
    sig = "render_always_on_catalog(entries_or_registry: list | dict, budget?: int) -> string",
    category = "skills"
)]
fn render_always_on_catalog_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let entries: Vec<VmValue> = match args.first() {
        Some(VmValue::List(list)) => list.iter().cloned().collect(),
        Some(VmValue::Dict(map)) => {
            vm_validate_registry("render_always_on_catalog", map)?;
            vm_skill_catalog_entries(vm_get_skills(map))
        }
        _ => {
            return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                "render_always_on_catalog: first argument must be a catalog entry list or skill registry",
            ))));
        }
    };
    let budget = match args.get(1) {
        Some(VmValue::Int(value)) if *value > 0 => *value as usize,
        Some(VmValue::Nil) | None => 2000usize,
        Some(_) => {
            return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                "render_always_on_catalog: second argument must be a positive integer budget",
            ))));
        }
    };
    let rendered = render_catalog(&entries, budget);
    Ok(VmValue::String(std::sync::Arc::from(rendered.as_str())))
}

#[harn_builtin(
    sig = "skill_find(registry: dict, name: string) -> dict?",
    category = "skills"
)]
fn skill_find_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    if args.len() < 2 {
        return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
            "skill_find: requires registry and name",
        ))));
    }
    let registry = match &args[0] {
        VmValue::Dict(map) => map,
        _ => {
            return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                "skill_find: first argument must be a skill registry",
            ))));
        }
    };
    vm_validate_registry("skill_find", registry)?;

    let target_name = args[1].display();
    for skill in vm_get_skills(registry) {
        if let VmValue::Dict(entry) = skill {
            if let Some(VmValue::String(name)) = entry.get("name") {
                if &**name == target_name.as_str() {
                    return Ok(skill.clone());
                }
            }
        }
    }
    Ok(VmValue::Nil)
}

#[harn_builtin(
    sig = "skill_select(registry: dict, names: list) -> dict",
    category = "skills"
)]
fn skill_select_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    if args.len() < 2 {
        return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
            "skill_select: requires registry and names list",
        ))));
    }
    let registry = match &args[0] {
        VmValue::Dict(map) => map,
        _ => {
            return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                "skill_select: first argument must be a skill registry",
            ))));
        }
    };
    vm_validate_registry("skill_select", registry)?;
    let names = match &args[1] {
        VmValue::List(list) => list
            .iter()
            .map(|value| value.display())
            .collect::<std::collections::BTreeSet<_>>(),
        _ => {
            return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                "skill_select: second argument must be a list of skill names",
            ))));
        }
    };

    let selected: Vec<VmValue> = vm_get_skills(registry)
        .iter()
        .filter(|skill| {
            skill
                .as_dict()
                .and_then(|entry| entry.get("name"))
                .map(|name| names.contains(&name.display()))
                .unwrap_or(false)
        })
        .cloned()
        .collect();

    let mut new_registry = (**registry).clone();
    new_registry.insert(
        "skills".to_string(),
        VmValue::List(std::sync::Arc::new(selected)),
    );
    Ok(VmValue::dict(new_registry))
}

#[harn_builtin(sig = "skill_describe(registry: dict) -> string", category = "skills")]
fn skill_describe_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let registry = match args.first() {
        Some(VmValue::Dict(map)) => map,
        _ => {
            return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                "skill_describe: requires a skill registry",
            ))));
        }
    };
    vm_validate_registry("skill_describe", registry)?;

    let skills = vm_get_skills(registry);

    if skills.is_empty() {
        return Ok(VmValue::String(std::sync::Arc::from(
            "Available skills:\n(none)",
        )));
    }

    let mut infos: Vec<(String, String, String)> = Vec::new();
    for skill in skills {
        if let VmValue::Dict(entry) = skill {
            let name = entry.get("name").map(|v| v.display()).unwrap_or_default();
            let description = entry
                .get("description")
                .map(|v| v.display())
                .unwrap_or_default();
            let when = entry
                .get("when_to_use")
                .map(|v| v.display())
                .unwrap_or_default();
            infos.push((name, description, when));
        }
    }
    infos.sort_by(|a, b| a.0.cmp(&b.0));

    let mut lines = vec!["Available skills:".to_string()];
    for (name, desc, when) in &infos {
        if desc.is_empty() {
            lines.push(format!("- {name}"));
        } else {
            lines.push(format!("- {name}: {desc}"));
        }
        if !when.is_empty() {
            lines.push(format!("  when: {when}"));
        }
    }
    Ok(VmValue::String(std::sync::Arc::from(lines.join("\n"))))
}

#[harn_builtin(
    sig = "skill_remove(registry: dict, name: string) -> dict",
    category = "skills"
)]
fn skill_remove_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    if args.len() < 2 {
        return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
            "skill_remove: requires registry and name",
        ))));
    }
    let registry = match &args[0] {
        VmValue::Dict(map) => (**map).clone(),
        _ => {
            return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                "skill_remove: first argument must be a skill registry",
            ))));
        }
    };
    vm_validate_registry("skill_remove", &registry)?;

    let target_name = args[1].display();
    let skills = vm_get_skills(&registry).to_vec();
    let filtered: Vec<VmValue> = skills
        .into_iter()
        .filter(|skill| {
            if let VmValue::Dict(entry) = skill {
                if let Some(VmValue::String(name)) = entry.get("name") {
                    return &**name != target_name.as_str();
                }
            }
            true
        })
        .collect();

    let mut new_registry = registry;
    new_registry.insert(
        "skills".to_string(),
        VmValue::List(std::sync::Arc::new(filtered)),
    );
    Ok(VmValue::dict(new_registry))
}

// `skill_render(skill, args_list)` — substitute `$ARGUMENTS`, `$N`,
// `${HARN_SKILL_DIR}`, `${HARN_SESSION_ID}` in the skill body.
//
// Accepts a skill entry (as returned by `skill_find`) or a raw
// string body. Args is an optional list of strings; missing values
// pass through unchanged so authors can spot under-supplied calls.
#[harn_builtin(
    sig = "skill_render(skill: dict | string, arguments?: list) -> string",
    category = "skills"
)]
fn skill_render_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    if args.is_empty() {
        return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
            "skill_render: requires a skill entry or body string",
        ))));
    }
    let (body, skill_dir) = match &args[0] {
        VmValue::String(s) => (s.to_string(), None),
        VmValue::Dict(map) => {
            let body = map.get("body").map(|v| v.display()).unwrap_or_default();
            if !body.is_empty() {
                let dir = map
                    .get("skill_dir")
                    .map(|v| v.display())
                    .filter(|s| !s.is_empty());
                (body, dir)
            } else {
                let skill_id = crate::skills::skill_entry_id(map);
                let fetched = crate::skills::current_skill_registry()
                    .and_then(|binding| (binding.fetcher)(&skill_id).ok());
                let dir = fetched
                    .as_ref()
                    .and_then(|skill| skill.skill_dir.as_ref())
                    .map(|path| path.display().to_string());
                let body = fetched.map(|skill| skill.body).unwrap_or_default();
                (body, dir)
            }
        }
        _ => {
            return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                "skill_render: first argument must be a skill entry or a string body",
            ))));
        }
    };
    let arguments: Vec<String> = match args.get(1) {
        Some(VmValue::List(list)) => list.iter().map(|v| v.display()).collect(),
        Some(VmValue::Nil) | None => Vec::new(),
        Some(_) => {
            return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                "skill_render: second argument must be a list of strings",
            ))));
        }
    };
    let ctx = crate::skills::SubstitutionContext {
        arguments,
        skill_dir,
        session_id: std::env::var("HARN_SESSION_ID").ok(),
        extra_env: Default::default(),
    };
    let rendered = crate::skills::substitute_skill_body(&body, &ctx);
    Ok(VmValue::String(std::sync::Arc::from(rendered.as_str())))
}

#[harn_builtin(
    sig = "load_skill(request: string | dict) -> string",
    category = "skills",
    kind = "async"
)]
async fn load_skill_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let (requested, inline_options) = parse_load_skill_request(args.first())?;
    let call_options = parse_load_skill_options(args.get(1))?;
    let session_id = call_options
        .session_id
        .or(inline_options.session_id)
        .or_else(|| std::env::var("HARN_SESSION_ID").ok());
    let loaded = crate::skills::load_bound_skill_by_name_with_options(
        &requested,
        crate::skills::LoadSkillOptions {
            session_id,
            require_signature: call_options.require_signature || inline_options.require_signature,
            model_invocation: true,
        },
    )
    .map_err(crate::skills::tool_rejected_error)?;
    Ok(VmValue::String(std::sync::Arc::from(
        loaded.rendered_body.as_str(),
    )))
}

#[harn_builtin(sig = "skill_count(registry: dict) -> int", category = "skills")]
fn skill_count_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let registry = match args.first() {
        Some(VmValue::Dict(map)) => map,
        _ => {
            return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                "skill_count: requires a skill registry",
            ))));
        }
    };
    vm_validate_registry("skill_count", registry)?;
    let count = vm_get_skills(registry).len();
    Ok(VmValue::Int(count as i64))
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    &SKILL_REGISTRY_IMPL_DEF,
    &SKILL_DEFINE_IMPL_DEF,
    &SKILL_LIST_IMPL_DEF,
    &SKILLS_CATALOG_ENTRIES_IMPL_DEF,
    &SKILL_WHO_SIGNED_IMPL_DEF,
    &RENDER_ALWAYS_ON_CATALOG_IMPL_DEF,
    &SKILL_FIND_IMPL_DEF,
    &SKILL_SELECT_IMPL_DEF,
    &SKILL_DESCRIBE_IMPL_DEF,
    &SKILL_REMOVE_IMPL_DEF,
    &SKILL_RENDER_IMPL_DEF,
    &LOAD_SKILL_IMPL_DEF,
    &SKILL_COUNT_IMPL_DEF,
];

fn parse_load_skill_request(
    value: Option<&VmValue>,
) -> Result<(String, crate::skills::LoadSkillOptions), VmError> {
    let Some(value) = value else {
        return Err(crate::skills::skill_vm_error(
            "load_skill: requires a non-empty skill name",
        ));
    };
    let options = parse_load_skill_options(Some(value))?;
    let requested = match value {
        VmValue::Dict(map) => map
            .get("name")
            .or_else(|| map.get("id"))
            .map(|value| value.display())
            .unwrap_or_default(),
        VmValue::String(name) => name.to_string(),
        other => other.display(),
    };
    if requested.is_empty() {
        return Err(crate::skills::skill_vm_error(
            "load_skill: requires a non-empty skill name",
        ));
    }
    Ok((requested, options))
}

fn parse_load_skill_options(
    value: Option<&VmValue>,
) -> Result<crate::skills::LoadSkillOptions, VmError> {
    let mut options = crate::skills::LoadSkillOptions::default();
    let Some(value) = value else {
        return Ok(options);
    };
    let VmValue::Dict(map) = value else {
        return Ok(options);
    };
    if let Some(value) = map.get("require_signature") {
        match value {
            VmValue::Bool(flag) => options.require_signature = *flag,
            VmValue::Nil => {}
            _ => {
                return Err(crate::skills::skill_vm_error(
                    "load_skill: require_signature must be a bool",
                ));
            }
        }
    }
    if let Some(value) = map.get("session_id") {
        match value {
            VmValue::String(session_id) if !session_id.is_empty() => {
                options.session_id = Some(session_id.to_string());
            }
            VmValue::String(_) | VmValue::Nil => {}
            _ => {
                return Err(crate::skills::skill_vm_error(
                    "load_skill: session_id must be a string",
                ));
            }
        }
    }
    Ok(options)
}

#[cfg(test)]
mod tests {
    use super::{render_catalog, vm_skill_catalog_entries};
    use crate::value::VmValue;
    use std::collections::BTreeMap;

    #[test]
    fn catalog_entries_use_fully_qualified_ids_and_sort() {
        let skills = vec![
            VmValue::dict(BTreeMap::from([
                (
                    "name".to_string(),
                    VmValue::String(std::sync::Arc::from("beta")),
                ),
                (
                    "description".to_string(),
                    VmValue::String(std::sync::Arc::from("Second skill")),
                ),
            ])),
            VmValue::dict(BTreeMap::from([
                (
                    "name".to_string(),
                    VmValue::String(std::sync::Arc::from("deploy")),
                ),
                (
                    "namespace".to_string(),
                    VmValue::String(std::sync::Arc::from("acme/ops")),
                ),
                (
                    "description".to_string(),
                    VmValue::String(std::sync::Arc::from("Deploy service")),
                ),
                (
                    "when_to_use".to_string(),
                    VmValue::String(std::sync::Arc::from("Ship a release")),
                ),
            ])),
        ];

        let catalog = vm_skill_catalog_entries(&skills);
        assert_eq!(catalog.len(), 2);
        let first = catalog[0].as_dict().unwrap();
        let second = catalog[1].as_dict().unwrap();
        assert_eq!(first.get("name").unwrap().display(), "acme/ops/deploy");
        assert_eq!(second.get("name").unwrap().display(), "beta");
    }

    #[test]
    fn render_catalog_is_deterministic_and_budgeted() {
        let entries = vec![
            VmValue::dict(BTreeMap::from([
                (
                    "name".to_string(),
                    VmValue::String(std::sync::Arc::from("alpha")),
                ),
                (
                    "description".to_string(),
                    VmValue::String(std::sync::Arc::from("First skill")),
                ),
                (
                    "when_to_use".to_string(),
                    VmValue::String(std::sync::Arc::from("Use alpha first")),
                ),
            ])),
            VmValue::dict(BTreeMap::from([
                (
                    "name".to_string(),
                    VmValue::String(std::sync::Arc::from("beta")),
                ),
                (
                    "description".to_string(),
                    VmValue::String(std::sync::Arc::from("Second skill")),
                ),
                (
                    "when_to_use".to_string(),
                    VmValue::String(std::sync::Arc::from("Use beta second")),
                ),
            ])),
        ];

        let once = render_catalog(&entries, 10_000);
        let twice = render_catalog(&entries, 10_000);
        assert_eq!(once, twice);
        assert!(once.contains("- `alpha`: First skill"));
        let small = render_catalog(&entries, 160);
        assert!(small.contains("omitted to stay within budget"));
    }
}
