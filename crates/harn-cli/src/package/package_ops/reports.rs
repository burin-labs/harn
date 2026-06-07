use super::*;

#[derive(Debug, Clone, Serialize)]
pub struct PackageCheckReport {
    pub package_dir: String,
    pub manifest_path: String,
    pub name: Option<String>,
    pub version: Option<String>,
    pub errors: Vec<PackageCheckDiagnostic>,
    pub warnings: Vec<PackageCheckDiagnostic>,
    pub exports: Vec<PackageExportReport>,
    pub tools: Vec<PackageToolExportReport>,
    pub skills: Vec<PackageSkillExportReport>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PackageCheckDiagnostic {
    pub field: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PackageExportReport {
    pub name: String,
    pub path: String,
    pub symbols: Vec<PackageApiSymbol>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PackageToolExportReport {
    pub name: String,
    pub module: String,
    pub symbol: String,
    pub permissions: Vec<String>,
    pub host_requirements: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PackageSkillExportReport {
    pub name: String,
    pub path: String,
    pub permissions: Vec<String>,
    pub host_requirements: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PackageApiSymbol {
    pub kind: String,
    pub name: String,
    pub signature: String,
    pub docs: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PackagePackReport {
    pub package_dir: String,
    pub artifact_dir: String,
    pub dry_run: bool,
    pub files: Vec<String>,
    pub check: PackageCheckReport,
}

#[derive(Debug, Clone, Serialize)]
pub struct PackagePublishReport {
    pub dry_run: bool,
    pub registry: String,
    pub artifact_dir: String,
    pub files: Vec<String>,
    pub tag: String,
    pub sha: String,
    pub remote: String,
    pub index_repo: String,
    pub index_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index_pr_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index_diff: Option<String>,
    pub check: PackageCheckReport,
}

#[derive(Debug, Clone, Serialize)]
pub struct PackageListReport {
    pub manifest_path: String,
    pub lock_path: String,
    pub lock_present: bool,
    pub dependency_count: usize,
    pub packages: Vec<PackageListEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PackageListEntry {
    pub name: String,
    pub source: String,
    pub package_version: Option<String>,
    pub harn_compat: Option<String>,
    pub provenance: Option<String>,
    pub materialized: bool,
    pub integrity: String,
    pub exports: PackageLockExports,
    pub permissions: Vec<String>,
    pub host_requirements: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PackageDoctorReport {
    pub ok: bool,
    pub manifest_path: String,
    pub lock_path: String,
    pub diagnostics: Vec<PackageDoctorDiagnostic>,
    pub packages: Vec<PackageListEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PackageDoctorDiagnostic {
    pub severity: String,
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
}
