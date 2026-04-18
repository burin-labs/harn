//! `harn skills` CLI subcommands: `list`, `inspect`, `match`, `install`,
//! `new`. These sit on top of the layered discovery already implemented
//! in `harn_vm::skills` so what the user sees here is byte-for-byte the
//! registry that `harn run` / `harn test` / `harn check` hand to the VM.
//!
//! Install resolves a git URL or local path into
//! `.harn/skills-cache/<namespace?>/<name>/` — mirroring
//! `.harn/packages/` so the filesystem package walker picks it up on
//! the next run. `new` scaffolds a SKILL.md + skills directory with
//! sensible defaults.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::{fs, process};

use harn_vm::skills::{
    build_fs_discovery, default_system_dirs, default_user_dir, parse_env_skills_path,
    DiscoveryOptions, FsLayerConfig, Layer, LayeredDiscovery, ManifestSource, Skill,
    SkillManifestRef,
};

use crate::cli::{
    SkillsInspectArgs, SkillsInstallArgs, SkillsListArgs, SkillsMatchArgs, SkillsNewArgs,
};
use crate::package::{load_skills_config, resolve_skills_paths, SkillSourceEntry};
use crate::skill_loader::canonicalize_cli_dirs;

const SKILLS_CACHE_DIR: &str = ".harn/skills-cache";

pub(crate) fn run_list(args: &SkillsListArgs) {
    let discovery = build_discovery(&args.skill_dir, args.from.as_deref());
    let report = discovery.build_report();

    if args.json {
        let mut entries = Vec::new();
        for winner in &report.winners {
            entries.push(serde_json::json!({
                "id": winner.id,
                "layer": winner.layer.label(),
                "description": winner.manifest.description,
                "when_to_use": winner.manifest.when_to_use,
                "origin": winner.origin,
                "shadowed": false,
            }));
        }
        if args.all {
            for shadowed in &report.shadowed {
                entries.push(serde_json::json!({
                    "id": shadowed.id,
                    "layer": shadowed.loser.label(),
                    "winner_layer": shadowed.winner.label(),
                    "origin": shadowed.loser_origin,
                    "shadowed": true,
                }));
            }
        }
        for entry in &entries {
            println!("{entry}");
        }
        return;
    }

    if report.winners.is_empty() {
        println!("No skills resolved.");
        println!(
            "Hint: add skills via --skill-dir, HARN_SKILLS_PATH, .harn/skills/, or harn.toml."
        );
    } else {
        println!("Resolved skills ({}):", report.winners.len());
        let id_width = report.winners.iter().map(|w| w.id.len()).max().unwrap_or(4);
        for winner in &report.winners {
            let desc = &winner.manifest.description;
            let short = if desc.is_empty() {
                "(no description)".to_string()
            } else {
                truncate(desc, 60)
            };
            println!(
                "  {:<id_width$}  [{}]  {}",
                winner.id,
                winner.layer.label(),
                short,
                id_width = id_width
            );
        }
    }

    if !report.shadowed.is_empty() {
        println!();
        println!("Shadowed skills ({}):", report.shadowed.len());
        for entry in &report.shadowed {
            println!(
                "  {:<12} winner=[{}] hidden=[{}] origin={}",
                entry.id,
                entry.winner.label(),
                entry.loser.label(),
                entry.loser_origin
            );
        }
    }

    if !report.disabled_layers.is_empty() {
        println!();
        print!("Disabled layers: ");
        let labels: Vec<&str> = report.disabled_layers.iter().map(|l| l.label()).collect();
        println!("{}", labels.join(", "));
    }

    if !report.unknown_fields.is_empty() {
        println!();
        println!("Unknown frontmatter fields:");
        for (id, fields) in &report.unknown_fields {
            println!("  {id}: {}", fields.join(", "));
        }
    }
}

pub(crate) fn run_inspect(args: &SkillsInspectArgs) {
    let discovery = build_discovery(&args.skill_dir, args.from.as_deref());
    let skill = match discovery.fetch(&args.name) {
        Ok(skill) => skill,
        Err(err) => {
            eprintln!("error: {err}");
            process::exit(1);
        }
    };

    if args.json {
        let json = skill_to_json(&skill);
        println!(
            "{}",
            serde_json::to_string_pretty(&json).unwrap_or_else(|error| {
                eprintln!("error: failed to serialize skill: {error}");
                process::exit(1);
            })
        );
        return;
    }

    println!("id:          {}", skill.id());
    println!("name:        {}", skill.manifest.name);
    println!("layer:       {}", skill.layer.label());
    if let Some(ns) = &skill.namespace {
        println!("namespace:   {ns}");
    }
    if !skill.manifest.description.is_empty() {
        println!("description: {}", skill.manifest.description);
    }
    if let Some(when) = &skill.manifest.when_to_use {
        println!("when_to_use: {when}");
    }
    if let Some(dir) = &skill.skill_dir {
        println!("skill_dir:   {}", dir.display());
    }
    if !skill.manifest.allowed_tools.is_empty() {
        println!("allowed:     {}", skill.manifest.allowed_tools.join(", "));
    }
    if !skill.manifest.paths.is_empty() {
        println!("paths:       {}", skill.manifest.paths.join(", "));
    }
    if let Some(model) = &skill.manifest.model {
        println!("model:       {model}");
    }
    if let Some(effort) = &skill.manifest.effort {
        println!("effort:      {effort}");
    }
    if !skill.unknown_fields.is_empty() {
        println!("unknown:     {}", skill.unknown_fields.join(", "));
    }

    if let Some(dir) = &skill.skill_dir {
        let bundled = collect_bundled_files(dir);
        if !bundled.is_empty() {
            println!();
            println!("Bundled files:");
            for file in &bundled {
                println!("  {}", file.display());
            }
        }
    }

    println!();
    println!("---- SKILL.md body ----");
    print!("{}", skill.body);
    if !skill.body.ends_with('\n') {
        println!();
    }
}

pub(crate) fn run_match(args: &SkillsMatchArgs) {
    let discovery = build_discovery(&args.skill_dir, args.from.as_deref());
    let report = discovery.build_report();
    let mut skills: Vec<Skill> = Vec::new();
    for winner in &report.winners {
        if let Ok(skill) = discovery.fetch(&winner.id) {
            skills.push(skill);
        }
    }

    let ranked = rank_skills(&skills, &args.query, &args.working_files);
    let top: Vec<&RankedSkill> = ranked.iter().take(args.top_n.max(1)).collect();

    if args.json {
        let out: Vec<serde_json::Value> = top
            .iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.id,
                    "layer": r.layer.label(),
                    "score": r.score,
                    "reason": r.reason,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&out).unwrap_or_else(|error| {
                eprintln!("error: failed to serialize match results: {error}");
                process::exit(1);
            })
        );
        return;
    }

    if top.is_empty() {
        println!("No skills matched '{}'.", args.query);
        return;
    }

    println!("Match results for: {}", args.query);
    for (idx, cand) in top.iter().enumerate() {
        println!(
            "  {:>2}. {:<20}  score={:.3}  [{}]  {}",
            idx + 1,
            cand.id,
            cand.score,
            cand.layer.label(),
            cand.reason
        );
    }
}

pub(crate) fn run_install(args: &SkillsInstallArgs) {
    let cache_root = PathBuf::from(SKILLS_CACHE_DIR);
    if let Err(error) = fs::create_dir_all(&cache_root) {
        eprintln!("error: failed to create {SKILLS_CACHE_DIR}: {error}");
        process::exit(1);
    }

    let spec = args.spec.trim();
    let local_candidate = PathBuf::from(spec);
    let is_local = local_candidate.exists();

    let (dest_name, dest_dir) = if let Some(namespace) = args.namespace.as_deref() {
        let dir = cache_root.join(namespace);
        if let Err(error) = fs::create_dir_all(&dir) {
            eprintln!("error: failed to create {}: {error}", dir.display());
            process::exit(1);
        }
        (namespace.to_string(), dir)
    } else {
        ("".to_string(), cache_root.clone())
    };

    let default_name = args
        .name
        .clone()
        .or_else(|| derive_name_from_spec(spec))
        .unwrap_or_else(|| "skill".to_string());
    let install_dir = dest_dir.join(&default_name);

    if install_dir.exists() {
        println!(
            "refreshing {}",
            install_dir
                .strip_prefix(".")
                .unwrap_or(&install_dir)
                .display()
        );
        let _ = fs::remove_dir_all(&install_dir);
    } else {
        println!(
            "installing {} to {}",
            spec,
            install_dir
                .strip_prefix(".")
                .unwrap_or(&install_dir)
                .display()
        );
    }

    if is_local {
        if let Err(error) = copy_dir_all(&local_candidate, &install_dir) {
            eprintln!("error: failed to copy from {spec}: {error}");
            process::exit(1);
        }
    } else {
        let url = resolve_git_url(spec);
        let mut cmd = process::Command::new("git");
        cmd.args(["clone", "--depth", "1"]);
        if let Some(tag) = args.tag.as_deref() {
            cmd.args(["--branch", tag]);
        }
        cmd.arg(&url);
        cmd.arg(&install_dir);
        cmd.stdout(process::Stdio::null());
        cmd.stderr(process::Stdio::piped());
        match cmd.output() {
            Ok(output) if output.status.success() => {
                let _ = fs::remove_dir_all(install_dir.join(".git"));
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                eprintln!("error: git clone failed: {stderr}");
                process::exit(1);
            }
            Err(error) => {
                eprintln!("error: failed to run git: {error}");
                eprintln!("hint: make sure git is installed and in PATH.");
                process::exit(1);
            }
        }
    }

    update_skills_lock(
        &dest_name,
        &install_dir,
        spec,
        args.tag.as_deref(),
        is_local,
    );

    let namespace_note = if let Some(ns) = args.namespace.as_deref() {
        format!(" (namespace={ns})")
    } else {
        String::new()
    };
    println!(
        "installed{} — layer=package, path={}",
        namespace_note,
        install_dir.display()
    );
}

pub(crate) fn run_new(args: &SkillsNewArgs) {
    let dest = if let Some(dir) = args.dir.as_deref() {
        PathBuf::from(dir)
    } else {
        PathBuf::from(".harn/skills").join(&args.name)
    };
    if dest.exists() {
        if args.force {
            if let Err(error) = fs::remove_dir_all(&dest) {
                eprintln!(
                    "error: failed to clear existing skill at {}: {error}",
                    dest.display()
                );
                process::exit(1);
            }
        } else {
            eprintln!(
                "error: {} already exists. Pass --force to overwrite.",
                dest.display()
            );
            process::exit(1);
        }
    }
    if let Err(error) = fs::create_dir_all(&dest) {
        eprintln!("error: failed to create {}: {error}", dest.display());
        process::exit(1);
    }

    let description = args
        .description
        .clone()
        .unwrap_or_else(|| format!("{} skill", args.name));
    let skill_md = format!(
        "---\n\
name: {name}\n\
description: {description}\n\
# when_to_use: <one-line trigger hint for the matcher>\n\
# allowed_tools: []\n\
# paths: []\n\
---\n\
\n\
# {name}\n\
\n\
Write the skill body here. This is the content the agent sees when\n\
this skill activates. You can include:\n\
\n\
- Step-by-step playbooks\n\
- Example tool invocations\n\
- Context-specific reminders\n\
\n\
Substitutions like `$ARGUMENTS`, `$1`, and `${{HARN_SKILL_DIR}}` are\n\
expanded when the skill is activated. See docs/src/skills.md for\n\
the full reference.\n",
        name = args.name,
        description = description,
    );
    let skill_path = dest.join("SKILL.md");
    if let Err(error) = fs::write(&skill_path, skill_md) {
        eprintln!("error: failed to write {}: {error}", skill_path.display());
        process::exit(1);
    }

    let bundled_dir = dest.join("files");
    let _ = fs::create_dir_all(&bundled_dir);
    let readme = bundled_dir.join("README.md");
    let _ = fs::write(
        &readme,
        format!(
            "# {} bundled files\n\n\
Drop supporting documents, templates, or scripts into this folder.\n\
They are accessible to the skill body via `${{HARN_SKILL_DIR}}/files/<name>`.\n",
            args.name
        ),
    );

    println!("Scaffolded skill '{}' at {}", args.name, dest.display());
    println!("  SKILL.md");
    println!("  files/README.md");
    println!();
    println!(
        "Edit the SKILL.md frontmatter and body, then run `harn skills list` to verify it's picked up."
    );
}

// --- shared helpers --------------------------------------------------------

struct RankedSkill {
    id: String,
    layer: Layer,
    score: f64,
    reason: String,
}

fn build_discovery(cli_dirs: &[String], from: Option<&str>) -> LayeredDiscovery {
    let anchor = from
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    let cli_dirs = canonicalize_cli_dirs(cli_dirs, Some(&anchor));

    let mut cfg = FsLayerConfig {
        cli_dirs,
        ..FsLayerConfig::default()
    };

    if let Ok(raw) = std::env::var("HARN_SKILLS_PATH") {
        if !raw.is_empty() {
            cfg.env_dirs = parse_env_skills_path(&raw);
        }
    }

    if let Some(project_root) = harn_vm::stdlib::process::find_project_root(&anchor) {
        cfg.project_root = Some(project_root.clone());
        cfg.packages_dir = Some(project_root.join(".harn").join("packages"));
    }

    let resolved = load_skills_config(Some(&anchor));
    let mut options = DiscoveryOptions::default();
    if let Some(resolved) = resolved.as_ref() {
        cfg.manifest_paths.extend(resolve_skills_paths(resolved));
        cfg.manifest_sources
            .extend(resolved.sources.iter().filter_map(manifest_source_to_vm));
        for label in &resolved.config.disable {
            if let Some(layer) = Layer::from_label(label) {
                options.disabled_layers.push(layer);
            }
        }
        if !resolved.config.lookup_order.is_empty() {
            let ordered: Vec<Layer> = resolved
                .config
                .lookup_order
                .iter()
                .filter_map(|s| Layer::from_label(s))
                .collect();
            if !ordered.is_empty() {
                options.lookup_order = Some(ordered);
            }
        }
    }

    cfg.user_dir = default_user_dir();
    cfg.system_dirs = default_system_dirs();

    // Pull skills-cache entries (populated by `harn skills install`) into
    // the package layer so installed skills show up without asking users
    // to also run `harn install`.
    if let Some(project_root) = cfg.project_root.as_ref() {
        let cache = project_root.join(SKILLS_CACHE_DIR);
        if cache.is_dir() {
            walk_install_cache(&cache, &mut cfg.manifest_paths);
        }
    }

    build_fs_discovery(&cfg, options)
}

fn manifest_source_to_vm(entry: &SkillSourceEntry) -> Option<ManifestSource> {
    match entry {
        SkillSourceEntry::Fs { path, namespace } => Some(ManifestSource::Fs {
            path: PathBuf::from(path),
            namespace: namespace.clone(),
        }),
        SkillSourceEntry::Git { namespace, .. } => {
            namespace.as_ref().map(|ns| ManifestSource::Git {
                path: PathBuf::new(),
                namespace: Some(ns.clone()),
            })
        }
        SkillSourceEntry::Registry { .. } => None,
    }
}

fn walk_install_cache(cache_root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(cache_root) else {
        return;
    };
    let mut stack: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.path())
        .collect();
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    while let Some(dir) = stack.pop() {
        if !seen.insert(dir.clone()) {
            continue;
        }
        if dir.join("SKILL.md").is_file() {
            if let Some(parent) = dir.parent() {
                out.push(parent.to_path_buf());
            } else {
                out.push(dir.clone());
            }
            continue;
        }
        let Ok(children) = fs::read_dir(&dir) else {
            continue;
        };
        for child in children.flatten() {
            let path = child.path();
            if path.is_dir() {
                stack.push(path);
            }
        }
    }
    out.sort();
    out.dedup();
}

fn skill_to_json(skill: &Skill) -> serde_json::Value {
    serde_json::json!({
        "id": skill.id(),
        "name": skill.manifest.name,
        "description": skill.manifest.description,
        "when_to_use": skill.manifest.when_to_use,
        "layer": skill.layer.label(),
        "namespace": skill.namespace,
        "skill_dir": skill.skill_dir.as_ref().map(|p| p.display().to_string()),
        "allowed_tools": skill.manifest.allowed_tools,
        "paths": skill.manifest.paths,
        "model": skill.manifest.model,
        "effort": skill.manifest.effort,
        "shell": skill.manifest.shell,
        "agent": skill.manifest.agent,
        "context": skill.manifest.context,
        "user_invocable": skill.manifest.user_invocable,
        "disable_model_invocation": skill.manifest.disable_model_invocation,
        "unknown_fields": skill.unknown_fields,
        "body": skill.body,
    })
}

fn collect_bundled_files(skill_dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect_bundled_files_inner(skill_dir, skill_dir, &mut out);
    out.sort();
    out
}

fn collect_bundled_files_inner(root: &Path, cursor: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(cursor) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let rel = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
        if path.is_dir() {
            collect_bundled_files_inner(root, &path, out);
        } else if rel.file_name().and_then(|f| f.to_str()) != Some("SKILL.md") {
            out.push(rel);
        }
    }
}

fn derive_name_from_spec(spec: &str) -> Option<String> {
    let trimmed = spec
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .trim_end_matches(|c: char| c.is_whitespace());
    let last = trimmed
        .rsplit(['/', ':', '\\'])
        .find(|s| !s.is_empty())?
        .to_string();
    if last.is_empty() {
        None
    } else {
        Some(last)
    }
}

fn resolve_git_url(spec: &str) -> String {
    // `owner/repo` shorthand expands to github.com — matches cargo-install UX.
    if !spec.contains("://")
        && !spec.starts_with("git@")
        && spec.matches('/').count() == 1
        && !spec.starts_with('.')
        && !spec.starts_with('/')
    {
        return format!("https://github.com/{spec}.git");
    }
    spec.to_string()
}

fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let dest_path = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(&entry.path(), &dest_path)?;
        } else {
            fs::copy(entry.path(), &dest_path)?;
        }
    }
    Ok(())
}

fn update_skills_lock(
    name: &str,
    install_dir: &Path,
    spec: &str,
    tag: Option<&str>,
    is_local: bool,
) {
    let lock_path = PathBuf::from(".harn/skills-cache/skills.lock");
    if let Some(parent) = lock_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let prior = fs::read_to_string(&lock_path).unwrap_or_default();
    let mut sections: Vec<String> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    for line in prior.lines() {
        if line.starts_with("[[skill]]") {
            if !current.is_empty() {
                sections.push(current.join("\n"));
            }
            current = Vec::new();
            current.push(line.to_string());
        } else if !current.is_empty() {
            current.push(line.to_string());
        }
    }
    if !current.is_empty() {
        sections.push(current.join("\n"));
    }
    let mut filtered: Vec<String> = sections
        .into_iter()
        .filter(|section| !section.contains(&format!("name = \"{name}\"")))
        .collect();

    let mut entry = String::new();
    entry.push_str("[[skill]]\n");
    entry.push_str(&format!("name = \"{name}\"\n"));
    entry.push_str(&format!(
        "path = \"{}\"\n",
        install_dir.display().to_string().replace('"', "\\\"")
    ));
    if is_local {
        entry.push_str(&format!(
            "source = \"path://{}\"\n",
            spec.replace('"', "\\\"")
        ));
    } else {
        entry.push_str(&format!(
            "source = \"{}\"\n",
            resolve_git_url(spec).replace('"', "\\\"")
        ));
        if let Some(tag) = tag {
            entry.push_str(&format!("tag = \"{tag}\"\n"));
        }
    }
    filtered.push(entry);

    let mut out =
        String::from("# Auto-generated by `harn skills install`. Safe to commit or ignore.\n\n");
    for section in filtered {
        out.push_str(section.trim_end_matches('\n'));
        out.push_str("\n\n");
    }
    if let Err(error) = fs::write(&lock_path, out) {
        eprintln!("warning: failed to update {}: {error}", lock_path.display());
    }
}

/// Minimal scorer that mirrors the metadata strategy used by the agent
/// loop (BM25-ish keyword hits + path-glob matches + prompt-mention
/// boost). Kept self-contained here so the CLI doesn't depend on the
/// agent-loop internals, and so the output stays stable even if the
/// runtime matcher changes.
fn rank_skills(skills: &[Skill], prompt: &str, working_files: &[String]) -> Vec<RankedSkill> {
    let tokens: Vec<String> = prompt
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() > 2)
        .map(|t| t.to_lowercase())
        .collect();
    let prompt_lower = prompt.to_lowercase();
    let mut out = Vec::new();
    for skill in skills {
        let mut score = 0.0_f64;
        let mut reasons: Vec<String> = Vec::new();
        let description = skill.manifest.description.as_str();
        let when = skill.manifest.when_to_use.as_deref().unwrap_or("");
        let keyword_hits = count_hits(&tokens, description) + count_hits(&tokens, when);
        if keyword_hits > 0 {
            let bm25 = (keyword_hits as f64) / (keyword_hits as f64 + 1.5);
            score += bm25;
            reasons.push(format!("{keyword_hits} keyword hit(s)"));
        }
        if !skill.manifest.name.is_empty()
            && prompt_lower.contains(&skill.manifest.name.to_lowercase())
        {
            score += 2.0;
            reasons.push(format!("prompt mentions '{}'", skill.manifest.name));
        }
        let path_hits = count_path_hits(&skill.manifest.paths, working_files);
        if path_hits > 0 {
            score += 1.5 * (path_hits as f64);
            reasons.push(format!("{path_hits} path glob(s) matched"));
        }
        if score > 0.0 {
            out.push(RankedSkill {
                id: skill.id(),
                layer: skill.layer,
                score,
                reason: if reasons.is_empty() {
                    "matched".to_string()
                } else {
                    reasons.join("; ")
                },
            });
        }
    }
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out
}

fn count_hits(terms: &[String], haystack: &str) -> usize {
    if terms.is_empty() || haystack.is_empty() {
        return 0;
    }
    let lower = haystack.to_lowercase();
    terms.iter().filter(|t| lower.contains(t.as_str())).count()
}

fn count_path_hits(patterns: &[String], files: &[String]) -> usize {
    let mut hits = 0;
    for pat in patterns {
        for file in files {
            if glob_match(pat, file) {
                hits += 1;
                break;
            }
        }
    }
    hits
}

fn glob_match(pattern: &str, path: &str) -> bool {
    glob_match_inner(pattern.as_bytes(), 0, path.as_bytes(), 0)
}

fn glob_match_inner(pat: &[u8], mut pi: usize, path: &[u8], mut si: usize) -> bool {
    while pi < pat.len() {
        match pat[pi] {
            b'*' => {
                let double = pi + 1 < pat.len() && pat[pi + 1] == b'*';
                let next_pi = if double { pi + 2 } else { pi + 1 };
                let next_pi = if double && next_pi < pat.len() && pat[next_pi] == b'/' {
                    next_pi + 1
                } else {
                    next_pi
                };
                if next_pi >= pat.len() {
                    if double {
                        return true;
                    }
                    return !path[si..].contains(&b'/');
                }
                for try_si in si..=path.len() {
                    if !double && path[si..try_si].contains(&b'/') {
                        break;
                    }
                    if glob_match_inner(pat, next_pi, path, try_si) {
                        return true;
                    }
                }
                return false;
            }
            b'?' => {
                if si >= path.len() || path[si] == b'/' {
                    return false;
                }
                pi += 1;
                si += 1;
            }
            c => {
                if si >= path.len() || path[si] != c {
                    return false;
                }
                pi += 1;
                si += 1;
            }
        }
    }
    si == path.len()
}

fn truncate(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_string();
    }
    let truncated: String = value.chars().take(max.saturating_sub(1)).collect();
    format!("{truncated}…")
}

// Silence an unused-import warning when the public aliases aren't used.
#[allow(dead_code)]
fn _types(_: SkillManifestRef) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_name_extracts_repo_segment() {
        assert_eq!(
            derive_name_from_spec("https://github.com/acme/harn-skills.git"),
            Some("harn-skills".to_string())
        );
        assert_eq!(
            derive_name_from_spec("./local/path/deploy"),
            Some("deploy".to_string())
        );
        assert_eq!(derive_name_from_spec("acme/ops"), Some("ops".to_string()));
    }

    #[test]
    fn resolve_git_url_expands_shorthand() {
        assert_eq!(
            resolve_git_url("acme/ops"),
            "https://github.com/acme/ops.git"
        );
        assert_eq!(
            resolve_git_url("https://example.com/x.git"),
            "https://example.com/x.git"
        );
    }

    #[test]
    fn truncate_keeps_short_strings() {
        assert_eq!(truncate("short", 60), "short");
        let long = "x".repeat(65);
        let truncated = truncate(&long, 60);
        assert_eq!(truncated.chars().count(), 60);
        assert!(truncated.ends_with('…'));
    }

    #[test]
    fn glob_matches_expected() {
        assert!(glob_match("src/*.rs", "src/main.rs"));
        assert!(!glob_match("src/*.rs", "src/sub/main.rs"));
        assert!(glob_match("infra/**", "infra/terraform/main.tf"));
    }
}
