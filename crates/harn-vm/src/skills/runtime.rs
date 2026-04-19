use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

use crate::value::{VmError, VmValue};

use super::{skill_entry_to_vm, substitute_skill_body, Skill, SubstitutionContext};

pub type SkillFetcher = Arc<dyn Fn(&str) -> Result<Skill, String> + Send + Sync>;

#[derive(Clone)]
pub struct BoundSkillRegistry {
    pub registry: VmValue,
    pub fetcher: SkillFetcher,
}

pub struct LoadedSkill {
    pub id: String,
    pub entry: BTreeMap<String, VmValue>,
    pub rendered_body: String,
}

thread_local! {
    static CURRENT_SKILL_REGISTRY: RefCell<Option<BoundSkillRegistry>> = const { RefCell::new(None) };
}

pub fn install_current_skill_registry(
    binding: Option<BoundSkillRegistry>,
) -> Option<BoundSkillRegistry> {
    CURRENT_SKILL_REGISTRY.with(|slot| slot.replace(binding))
}

pub fn current_skill_registry() -> Option<BoundSkillRegistry> {
    CURRENT_SKILL_REGISTRY.with(|slot| slot.borrow().clone())
}

pub fn clear_current_skill_registry() {
    CURRENT_SKILL_REGISTRY.with(|slot| {
        *slot.borrow_mut() = None;
    });
}

pub fn skill_entry_id(entry: &BTreeMap<String, VmValue>) -> String {
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

pub fn resolve_skill_entry(
    registry: &VmValue,
    target: &str,
    builtin_name: &str,
) -> Result<BTreeMap<String, VmValue>, String> {
    let dict = registry
        .as_dict()
        .ok_or_else(|| format!("{builtin_name}: bound skill registry is not a dict"))?;
    let skills = match dict.get("skills") {
        Some(VmValue::List(list)) => list,
        _ => {
            return Err(format!("{builtin_name}: bound skill registry is malformed"));
        }
    };

    let mut bare_matches: Vec<BTreeMap<String, VmValue>> = Vec::new();
    for skill in skills.iter() {
        let Some(entry) = skill.as_dict() else {
            continue;
        };
        if skill_entry_id(entry) == target {
            return Ok(entry.clone());
        }
        if entry
            .get("name")
            .map(|value| value.display())
            .is_some_and(|name| name == target)
        {
            bare_matches.push(entry.clone());
        }
    }

    match bare_matches.len() {
        1 => Ok(bare_matches.remove(0)),
        0 => Err(format!("skill '{target}' not found")),
        _ => Err(format!(
            "skill '{target}' is ambiguous; use the fully qualified id from the catalog"
        )),
    }
}

fn entry_has_inline_body(entry: &BTreeMap<String, VmValue>) -> bool {
    entry
        .get("body")
        .map(|value| value.display())
        .filter(|value| !value.is_empty())
        .is_some()
        || entry
            .get("prompt")
            .map(|value| value.display())
            .filter(|value| !value.is_empty())
            .is_some()
}

fn body_from_entry(entry: &BTreeMap<String, VmValue>) -> String {
    entry
        .get("body")
        .map(|value| value.display())
        .filter(|value| !value.is_empty())
        .or_else(|| {
            entry
                .get("prompt")
                .map(|value| value.display())
                .filter(|value| !value.is_empty())
        })
        .unwrap_or_default()
}

fn hydrate_skill_entry(
    entry: BTreeMap<String, VmValue>,
    fetcher: Option<&SkillFetcher>,
    builtin_name: &str,
) -> Result<BTreeMap<String, VmValue>, String> {
    if entry_has_inline_body(&entry) {
        return Ok(entry);
    }

    let skill_id = skill_entry_id(&entry);
    let Some(fetcher) = fetcher else {
        return Err(format!(
            "{builtin_name}: skill '{skill_id}' is not lazily loadable in this scope"
        ));
    };

    let loaded = fetcher(&skill_id)?;
    match skill_entry_to_vm(&loaded) {
        VmValue::Dict(dict) => Ok((*dict).clone()),
        _ => Err(format!(
            "{builtin_name}: failed to hydrate skill '{skill_id}'"
        )),
    }
}

fn render_skill_entry(entry: &BTreeMap<String, VmValue>, session_id: Option<&str>) -> String {
    let skill_dir = entry
        .get("skill_dir")
        .map(|value| value.display())
        .filter(|value| !value.is_empty());
    substitute_skill_body(
        &body_from_entry(entry),
        &SubstitutionContext {
            arguments: Vec::new(),
            skill_dir,
            session_id: session_id.map(str::to_string),
            extra_env: Default::default(),
        },
    )
}

pub fn load_bound_skill_by_name(
    requested: &str,
    session_id: Option<&str>,
) -> Result<LoadedSkill, String> {
    let Some(binding) = current_skill_registry() else {
        return Err(
            "load_skill: no skill registry is bound to this scope. Start the VM with discovered skills first."
                .to_string(),
        );
    };
    load_skill_from_registry(
        &binding.registry,
        Some(&binding.fetcher),
        requested,
        session_id,
        "load_skill",
    )
}

pub fn load_skill_from_registry(
    registry: &VmValue,
    fetcher: Option<&SkillFetcher>,
    requested: &str,
    session_id: Option<&str>,
    builtin_name: &str,
) -> Result<LoadedSkill, String> {
    let entry = resolve_skill_entry(registry, requested, builtin_name)?;
    let entry = hydrate_skill_entry(entry, fetcher, builtin_name)?;
    let id = skill_entry_id(&entry);
    let rendered_body = render_skill_entry(&entry, session_id);
    Ok(LoadedSkill {
        id,
        entry,
        rendered_body,
    })
}

pub fn vm_error(message: impl Into<String>) -> VmError {
    VmError::Thrown(VmValue::String(Rc::from(message.into())))
}
