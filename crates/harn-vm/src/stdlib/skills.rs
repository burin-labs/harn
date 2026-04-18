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

use std::collections::BTreeMap;
use std::rc::Rc;

use crate::value::{VmError, VmValue};
use crate::vm::Vm;

fn vm_validate_registry(name: &str, dict: &BTreeMap<String, VmValue>) -> Result<(), VmError> {
    match dict.get("_type") {
        Some(VmValue::String(t)) if &**t == "skill_registry" => Ok(()),
        _ => Err(VmError::Thrown(VmValue::String(Rc::from(format!(
            "{name}: argument must be a skill registry (created with skill_registry())"
        ))))),
    }
}

fn vm_get_skills(dict: &BTreeMap<String, VmValue>) -> &[VmValue] {
    match dict.get("skills") {
        Some(VmValue::List(list)) => list,
        _ => &[],
    }
}

pub(crate) fn register_skill_builtins(vm: &mut Vm) {
    vm.register_builtin("skill_registry", |_args, _out| {
        let mut registry = BTreeMap::new();
        registry.insert(
            "_type".to_string(),
            VmValue::String(Rc::from("skill_registry")),
        );
        registry.insert("skills".to_string(), VmValue::List(Rc::new(Vec::new())));
        Ok(VmValue::Dict(Rc::new(registry)))
    });

    vm.register_builtin("skill_define", |args, _out| {
        if args.len() < 3 {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "skill_define: requires registry, name, and config dict",
            ))));
        }
        let registry = match &args[0] {
            VmValue::Dict(map) => (**map).clone(),
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
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
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "skill_define: skill name must be a non-empty string",
            ))));
        }

        let config = match &args[2] {
            VmValue::Dict(map) => (**map).clone(),
            VmValue::Nil => BTreeMap::new(),
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "skill_define: third argument must be a config dict",
                ))));
            }
        };

        // Light validation on known string keys: enforce stable error
        // messages when a user mis-types a value.
        for key in [
            "description",
            "when_to_use",
            "prompt",
            "invocation",
            "model",
            "effort",
        ] {
            if let Some(value) = config.get(key) {
                if !matches!(value, VmValue::String(_) | VmValue::Nil) {
                    return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                        "skill_define: '{key}' must be a string"
                    )))));
                }
            }
        }
        for key in ["paths", "allowed_tools", "mcp"] {
            if let Some(value) = config.get(key) {
                if !matches!(value, VmValue::List(_) | VmValue::Nil) {
                    return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                        "skill_define: '{key}' must be a list"
                    )))));
                }
            }
        }

        let mut entry = BTreeMap::new();
        entry.insert("name".to_string(), VmValue::String(Rc::from(name.as_str())));
        // Keep `description` at the top level even if missing (empty string)
        // so `skill_describe` / transcript surfaces have a stable shape.
        if !config.contains_key("description") {
            entry.insert("description".to_string(), VmValue::String(Rc::from("")));
        }
        for (k, v) in config.iter() {
            entry.insert(k.clone(), v.clone());
        }
        let entry_value = VmValue::Dict(Rc::new(entry));

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
        new_registry.insert("skills".to_string(), VmValue::List(Rc::new(new_skills)));
        Ok(VmValue::Dict(Rc::new(new_registry)))
    });

    vm.register_builtin("skill_list", |args, _out| {
        let registry = match args.first() {
            Some(VmValue::Dict(map)) => map,
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
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
                result.push(VmValue::Dict(Rc::new(desc)));
            }
        }
        Ok(VmValue::List(Rc::new(result)))
    });

    vm.register_builtin("skill_find", |args, _out| {
        if args.len() < 2 {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "skill_find: requires registry and name",
            ))));
        }
        let registry = match &args[0] {
            VmValue::Dict(map) => map,
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
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
    });

    vm.register_builtin("skill_select", |args, _out| {
        if args.len() < 2 {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "skill_select: requires registry and names list",
            ))));
        }
        let registry = match &args[0] {
            VmValue::Dict(map) => map,
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
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
                return Err(VmError::Thrown(VmValue::String(Rc::from(
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
        new_registry.insert("skills".to_string(), VmValue::List(Rc::new(selected)));
        Ok(VmValue::Dict(Rc::new(new_registry)))
    });

    vm.register_builtin("skill_describe", |args, _out| {
        let registry = match args.first() {
            Some(VmValue::Dict(map)) => map,
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "skill_describe: requires a skill registry",
                ))));
            }
        };
        vm_validate_registry("skill_describe", registry)?;

        let skills = vm_get_skills(registry);

        if skills.is_empty() {
            return Ok(VmValue::String(Rc::from("Available skills:\n(none)")));
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
        Ok(VmValue::String(Rc::from(lines.join("\n"))))
    });

    vm.register_builtin("skill_remove", |args, _out| {
        if args.len() < 2 {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "skill_remove: requires registry and name",
            ))));
        }
        let registry = match &args[0] {
            VmValue::Dict(map) => (**map).clone(),
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
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
        new_registry.insert("skills".to_string(), VmValue::List(Rc::new(filtered)));
        Ok(VmValue::Dict(Rc::new(new_registry)))
    });

    vm.register_builtin("skill_count", |args, _out| {
        let registry = match args.first() {
            Some(VmValue::Dict(map)) => map,
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "skill_count: requires a skill registry",
                ))));
            }
        };
        vm_validate_registry("skill_count", registry)?;
        let count = vm_get_skills(registry).len();
        Ok(VmValue::Int(count as i64))
    });
}
