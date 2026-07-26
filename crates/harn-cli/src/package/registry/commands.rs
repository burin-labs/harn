//! The `harn package`/`harn registry` CLI entry points — argument handling,
//! human and JSON rendering, and process exit codes.

use crate::package::*;

pub fn list_package_cache() {
    let result = (|| -> Result<(PathBuf, Vec<PackageCacheEntry>), PackageError> {
        Ok((cache_root()?, discover_package_cache_entries()?))
    })();

    match result {
        Ok((root, entries)) => {
            println!("Cache root: {}", root.display());
            if entries.is_empty() {
                println!("No cached packages.");
                return;
            }
            println!("kind\tkey\tcontent_hash\tsource\tpath");
            for entry in entries {
                let (source, content_hash) = entry
                    .metadata
                    .as_ref()
                    .map(|metadata| (metadata.source.as_str(), metadata.content_hash.as_str()))
                    .unwrap_or(("(unknown)", "(unknown)"));
                println!(
                    "{}\t{}\t{}\t{}\t{}",
                    entry.kind,
                    entry.commit,
                    content_hash,
                    source,
                    entry.path.display()
                );
            }
        }
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
}

pub fn clean_package_cache(all: bool) {
    match clean_package_cache_impl(all) {
        Ok(removed) => println!("Removed {removed} cached package entries."),
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
}

pub fn verify_package_cache(materialized: bool) {
    match verify_package_cache_impl(materialized) {
        Ok(verified) => println!("Verified {verified} package cache entries."),
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
}

pub fn search_package_registry(query: Option<&str>, registry: Option<&str>, json: bool) {
    match search_package_registry_impl(query, registry) {
        Ok(packages) if json => {
            println!(
                "{}",
                serde_json::to_string_pretty(&packages)
                    .unwrap_or_else(|error| format!(r#"{{"error":"{error}"}}"#))
            );
        }
        Ok(packages) => {
            if packages.is_empty() {
                println!("No packages found.");
                return;
            }
            println!("name\tlatest\tharn\tcontract\tdescription");
            for package in packages {
                let latest = latest_registry_version(&package)
                    .map(|version| version.version.as_str())
                    .unwrap_or("-");
                println!(
                    "{}\t{}\t{}\t{}\t{}",
                    package.name,
                    latest,
                    package.harn.as_deref().unwrap_or("-"),
                    package.connector_contract.as_deref().unwrap_or("-"),
                    package.description.as_deref().unwrap_or("")
                );
            }
        }
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
}

pub fn search_rule_package_registry(query: Option<&str>, registry: Option<&str>, json: bool) {
    match search_rule_package_registry_impl(query, registry) {
        Ok(packages) if json => {
            println!(
                "{}",
                serde_json::to_string_pretty(&packages)
                    .unwrap_or_else(|error| format!(r#"{{"error":"{error}"}}"#))
            );
        }
        Ok(packages) => {
            if packages.is_empty() {
                println!("No rule packs found.");
                return;
            }
            println!("name\tlatest\tlanguages\trules\tsafety\tdescription");
            for package in packages {
                let latest = latest_registry_version(&package)
                    .map(|version| version.version.as_str())
                    .unwrap_or("-")
                    .to_string();
                let rule_pack = package.rule_pack.unwrap_or_default();
                let languages = if rule_pack.languages.is_empty() {
                    "-".to_string()
                } else {
                    rule_pack.languages.join(",")
                };
                let safety = if rule_pack.safety_summary.is_empty() {
                    "-".to_string()
                } else {
                    rule_pack.safety_summary.join(",")
                };
                println!(
                    "{}\t{}\t{}\t{}\t{}\t{}",
                    package.name,
                    latest,
                    languages,
                    rule_pack.rule_count,
                    safety,
                    package.description.as_deref().unwrap_or("")
                );
            }
        }
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
}

pub fn show_package_registry_info(spec: &str, registry: Option<&str>, json: bool) {
    match package_registry_info_impl(spec, registry) {
        Ok(info) if json => {
            println!(
                "{}",
                serde_json::to_string_pretty(&info)
                    .unwrap_or_else(|error| format!(r#"{{"error":"{error}"}}"#))
            );
        }
        Ok(info) => {
            let package = info.package;
            println!("{}", package.name);
            if let Some(description) = package.description.as_deref() {
                println!("description: {description}");
            }
            println!("repository: {}", package.repository);
            if let Some(license) = package.license.as_deref() {
                println!("license: {license}");
            }
            if let Some(harn) = package.harn.as_deref() {
                println!("harn: {harn}");
            }
            if let Some(contract) = package.connector_contract.as_deref() {
                println!("connector_contract: {contract}");
            }
            if let Some(docs) = package.docs_url.as_deref() {
                println!("docs: {docs}");
            }
            if let Some(checksum) = package.checksum.as_deref() {
                println!("checksum: {checksum}");
            }
            if let Some(provenance) = package.provenance.as_deref() {
                println!("provenance: {provenance}");
            }
            if !package.exports.is_empty() {
                println!("exports: {}", package.exports.join(", "));
            }
            if let Some(rule_pack) = package.rule_pack.as_ref() {
                println!("rule_pack: yes");
                println!("rules: {}", rule_pack.rule_count);
                if !rule_pack.languages.is_empty() {
                    println!("languages: {}", rule_pack.languages.join(", "));
                }
                if !rule_pack.safety_summary.is_empty() {
                    println!("safety: {}", rule_pack.safety_summary.join(", "));
                }
            }
            if let Some(version) = info.selected_version {
                println!("selected: {}", version.version);
                if let Some(git) = version.git.as_deref() {
                    println!("git: {git}");
                }
                if let Some(archive) = version.archive.as_deref() {
                    println!("archive: {archive}");
                }
                if let Some(rev) = version.rev.as_deref() {
                    println!("rev: {rev}");
                }
                if let Some(branch) = version.branch.as_deref() {
                    println!("branch: {branch}");
                }
                if let Some(package_name) = version.package.as_deref() {
                    println!("package: {package_name}");
                }
            }
            if !package.versions.is_empty() {
                let versions = package
                    .versions
                    .iter()
                    .map(|version| {
                        if version.yanked {
                            format!("{} (yanked)", version.version)
                        } else {
                            version.version.clone()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                println!("versions: {versions}");
            }
        }
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
}
