//! The package-export surface a manifest declares, projected into the shape
//! the lock file records and consumers read back.

use crate::package::*;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageLockExports {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modules: Vec<PackageLockExport>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<PackageLockExport>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<PackageLockExport>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub personas: Vec<String>,
}

impl PackageLockExports {
    pub(crate) fn is_empty(&self) -> bool {
        self.modules.is_empty()
            && self.tools.is_empty()
            && self.skills.is_empty()
            && self.personas.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageLockExport {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
}

pub(crate) fn package_lock_exports_from_manifest(manifest: &Manifest) -> PackageLockExports {
    let mut modules: Vec<PackageLockExport> = manifest
        .exports
        .iter()
        .map(|(name, path)| PackageLockExport {
            name: name.clone(),
            path: Some(path.clone()),
            symbol: None,
        })
        .collect();
    modules.sort_by(|left, right| left.name.cmp(&right.name));

    let (mut tools, mut skills) = manifest
        .package
        .as_ref()
        .map(|package| {
            let tools = package
                .tools
                .iter()
                .map(|tool| PackageLockExport {
                    name: tool.name.clone(),
                    path: Some(tool.module.clone()),
                    symbol: Some(tool.symbol.clone()),
                })
                .collect::<Vec<_>>();
            let skills = package
                .skills
                .iter()
                .map(|skill| PackageLockExport {
                    name: skill.name.clone(),
                    path: Some(skill.path.clone()),
                    symbol: None,
                })
                .collect::<Vec<_>>();
            (tools, skills)
        })
        .unwrap_or_default();
    tools.sort_by(|left, right| left.name.cmp(&right.name));
    skills.sort_by(|left, right| left.name.cmp(&right.name));

    let mut personas: Vec<String> = manifest
        .personas
        .iter()
        .filter_map(|persona| persona.name.clone())
        .collect();
    personas.sort();
    personas.dedup();

    PackageLockExports {
        modules,
        tools,
        skills,
        personas,
    }
}

pub(crate) fn normalized_requirements(values: &[String]) -> Vec<String> {
    let mut out: Vec<String> = values
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect();
    out.sort();
    out.dedup();
    out
}
