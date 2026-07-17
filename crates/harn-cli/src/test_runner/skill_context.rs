//! Suite-owned skill discovery shared by hermetic per-case VMs.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::TestCase;

pub(super) struct PreparedSkillContexts {
    by_source_dir: BTreeMap<PathBuf, Arc<crate::skill_loader::LoadedSkills>>,
}

impl PreparedSkillContexts {
    pub(super) fn prepare(cases: &[TestCase], cli_skill_dirs: &[PathBuf]) -> Self {
        let mut by_source_dir = BTreeMap::new();
        for case in cases {
            let source_dir = source_dir(case);
            by_source_dir.entry(source_dir).or_insert_with(|| {
                Arc::new(crate::skill_loader::load_skills(
                    &crate::skill_loader::SkillLoaderInputs {
                        cli_dirs: cli_skill_dirs.to_vec(),
                        source_path: Some(case.file.clone()),
                    },
                ))
            });
        }
        for loaded in by_source_dir.values() {
            crate::skill_loader::emit_loader_warnings(&loaded.loader_warnings);
        }
        Self { by_source_dir }
    }

    pub(super) fn for_case(&self, case: &TestCase) -> &crate::skill_loader::LoadedSkills {
        self.by_source_dir
            .get(&source_dir(case))
            .expect("every discovered test directory has a prepared skill context")
    }
}

fn source_dir(case: &TestCase) -> PathBuf {
    case.file
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf()
}
