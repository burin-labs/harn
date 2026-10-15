#[cfg(target_os = "linux")]
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[cfg(target_os = "linux")]
use super::IsolatedPythonSourceVerifier;
#[cfg(target_os = "linux")]
use super::VerifierLoaderObservation;

/// Ordered, host-admitted roots replace ambient Python search paths. Startup
/// hooks, bytecode-only modules, native extensions and namespace packages are
/// outside this source-loader contract.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IsolatedPythonModuleVerifier {
    pub id: String,
    pub interpreter: PathBuf,
    pub module: String,
    pub search_roots: Vec<PathBuf>,
    /// Additional source modules or packages captured with the entrypoint.
    #[serde(default)]
    pub dependencies: Vec<String>,
    pub workspace: PathBuf,
    pub args: Vec<String>,
}

impl IsolatedPythonModuleVerifier {
    pub fn invocation_args(&self) -> Vec<String> {
        let mut args = vec!["-I".into(), "-S".into(), "-m".into(), self.module.clone()];
        args.extend(self.args.clone());
        args
    }

    pub fn matches(&self, program: &str, args: &[String], cwd: &Path) -> bool {
        Path::new(program) == self.interpreter
            && args == self.invocation_args()
            && cwd == self.workspace
    }

    #[cfg(target_os = "linux")]
    pub(super) fn capture(&self) -> Result<CapturedModules, VerifierLoaderObservation> {
        if self.search_roots.is_empty() || !valid_module_name(&self.module) {
            return Err(VerifierLoaderObservation::Refused {
                reason: "Python module verifier requires a module name and explicit search roots"
                    .into(),
            });
        }
        let mut result = CapturedModules {
            entry: self.module.clone(),
            modules: BTreeMap::new(),
            candidates: BTreeMap::new(),
            source_bytes: 0,
        };
        for root in &self.search_roots {
            if !root.is_absolute()
                || root.canonicalize().map_err(unmeasured)? != *root
                || !root.is_dir()
            {
                return Err(VerifierLoaderObservation::Refused {
                    reason: "Python verifier search roots must be canonical absolute directories"
                        .into(),
                });
            }
        }
        for module in std::iter::once(&self.module).chain(self.dependencies.iter()) {
            if !valid_module_name(module) {
                return Err(VerifierLoaderObservation::Refused {
                    reason: "invalid Python verifier dependency name".into(),
                });
            }
            let top = module.split('.').next().unwrap();
            if result.modules.contains_key(module) {
                continue;
            }
            for root in &self.search_roots {
                let package = root.join(top);
                let source = root.join(format!("{top}.py"));
                let package_source = package.join("__init__.py");
                let package_present = source_present(&package_source)?;
                let module_present = source_present(&source)?;
                if !package_present {
                    result.candidates.insert(package_source, None);
                }
                if !module_present {
                    result.candidates.insert(source.clone(), None);
                }
                if package_present {
                    collect_package(&package, top, &mut result).map_err(unmeasured)?;
                    break;
                }
                if module_present {
                    result
                        .insert(
                            top.into(),
                            capture_module(&source, false).map_err(unmeasured)?,
                        )
                        .map_err(unmeasured)?;
                    break;
                }
                if package.exists() || root.join(format!("{top}.pyc")).exists() {
                    return Err(unmeasured(
                        "Python namespace or bytecode-only verifier loading is unsupported",
                    ));
                }
            }
            if !result.modules.contains_key(module) {
                return Err(VerifierLoaderObservation::Absent {
                    paths: self
                        .search_roots
                        .iter()
                        .map(|root| root.join(module.replace('.', "/")))
                        .collect(),
                });
            }
        }
        let selected =
            result
                .modules
                .get(&self.module)
                .ok_or_else(|| VerifierLoaderObservation::Absent {
                    paths: self
                        .search_roots
                        .iter()
                        .map(|root| root.join(self.module.replace('.', "/")))
                        .collect(),
                })?;
        if selected.package
            && !result
                .modules
                .contains_key(&format!("{}.__main__", self.module))
        {
            return Err(VerifierLoaderObservation::Absent {
                paths: vec![selected.origin.parent().unwrap().join("__main__.py")],
            });
        }
        let entry = if selected.package {
            &result.modules[&format!("{}.__main__", self.module)]
        } else {
            selected
        };
        if entry.source.is_empty() {
            return Err(VerifierLoaderObservation::Refused {
                reason: "Python verifier entrypoint is empty".into(),
            });
        }
        Ok(result)
    }
}

#[cfg(target_os = "linux")]
fn unmeasured(error: impl ToString) -> VerifierLoaderObservation {
    VerifierLoaderObservation::Unmeasured {
        reason: error.to_string(),
    }
}

#[cfg(target_os = "linux")]
fn source_present(path: &Path) -> Result<bool, VerifierLoaderObservation> {
    match path.metadata() {
        Ok(metadata) if metadata.is_file() => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(unmeasured(error)),
        Ok(_) => Err(unmeasured("Python verifier source is not a regular file")),
    }
}

#[cfg(target_os = "linux")]
#[derive(Debug, Serialize)]
pub(super) struct CapturedModules {
    pub(super) entry: String,
    pub(super) modules: BTreeMap<String, CapturedModule>,
    #[serde(skip)]
    candidates: BTreeMap<PathBuf, Option<super::MaterialIdentity>>,
    #[serde(skip)]
    source_bytes: usize,
}

#[cfg(target_os = "linux")]
impl CapturedModules {
    fn insert(&mut self, name: String, module: CapturedModule) -> Result<(), String> {
        if self.modules.len() >= super::limits().module_count
            || module.source.len()
                > super::limits()
                    .bundle_bytes
                    .saturating_sub(self.source_bytes)
        {
            return Err("Python verifier package exceeds capture limits".into());
        }
        self.source_bytes += module.source.len();
        self.modules.insert(name, module);
        Ok(())
    }
    pub(super) fn extend_baseline(
        &self,
        baseline: &mut BTreeMap<PathBuf, Option<super::MaterialIdentity>>,
    ) {
        baseline.extend(self.candidates.clone());
        for module in self.modules.values() {
            baseline.insert(
                module.origin.clone(),
                Some(super::MaterialIdentity::of(&module.source)),
            );
        }
    }
    pub(super) fn source_request(
        &self,
        request: &IsolatedPythonModuleVerifier,
    ) -> IsolatedPythonSourceVerifier {
        let selected = &self.modules[&request.module];
        let entry = if selected.package {
            &self.modules[&format!("{}.__main__", request.module)]
        } else {
            selected
        };
        IsolatedPythonSourceVerifier {
            id: request.id.clone(),
            interpreter: request.interpreter.clone(),
            source: entry.origin.clone(),
            workspace: request.workspace.clone(),
            args: request.args.clone(),
        }
    }
}

#[cfg(target_os = "linux")]
#[derive(Debug, Serialize)]
pub(super) struct CapturedModule {
    origin: PathBuf,
    source: Vec<u8>,
    package: bool,
}

#[cfg(target_os = "linux")]
fn valid_module_name(name: &str) -> bool {
    !name.is_empty() && name.split('.').all(valid_identifier)
}

#[cfg(target_os = "linux")]
fn valid_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c == '_' || c.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

#[cfg(target_os = "linux")]
fn capture_module(path: &Path, package: bool) -> Result<CapturedModule, String> {
    if path
        .symlink_metadata()
        .map_err(|e| e.to_string())?
        .file_type()
        .is_symlink()
    {
        return Err("symlinked Python verifier material is unmeasured".into());
    }
    Ok(CapturedModule {
        origin: path.to_path_buf(),
        source: super::bounded_read(path, super::limits().source_bytes)?,
        package,
    })
}

#[cfg(target_os = "linux")]
fn collect_package(path: &Path, name: &str, modules: &mut CapturedModules) -> Result<(), String> {
    if path
        .symlink_metadata()
        .map_err(|e| e.to_string())?
        .file_type()
        .is_symlink()
    {
        return Err("symlinked Python verifier packages are unmeasured".into());
    }
    modules.insert(
        name.into(),
        capture_module(&path.join("__init__.py"), true)?,
    )?;
    for entry in std::fs::read_dir(path).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if path.extension().is_some_and(|extension| extension == "py")
            && stem != "__init__"
            && valid_identifier(stem)
        {
            modules.insert(format!("{name}.{stem}"), capture_module(&path, false)?)?;
        } else if path.is_dir() && path.join("__init__.py").is_file() {
            let name_part = path
                .file_name()
                .and_then(|part| part.to_str())
                .unwrap_or("");
            if valid_identifier(name_part) {
                collect_package(&path, &format!("{name}.{name_part}"), modules)?;
            }
        }
    }
    Ok(())
}
