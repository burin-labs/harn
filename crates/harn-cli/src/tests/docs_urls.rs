//! Every `harnlang.com` documentation link Harn prints must resolve.
//!
//! `harn doctor` shipped `https://harnlang.com/docs/llm/providers.html` and
//! `https://harnlang.com/docs/protocol-artifacts.html` for months. Both are
//! 404s: the docs site publishes `docs/src/<page>.md` at `/<page>.html`, with
//! no `/docs/` prefix. A person who follows a diagnostic's "read more" link
//! and lands on a 404 is worse off than one who got no link at all, and the
//! mistake is invisible in review because the URL looks plausible.
//!
//! This walks the shipped Rust sources and holds every documentation URL to
//! the site's actual layout, offline.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Pages the site build publishes outside the `docs/src` tree. Keep this
/// list at exactly what `scripts/build_docs_site.sh` copies by hand.
const PRERENDERED_EXCEPTIONS: &[&str] = &[
    "docs/llm/harn-quickref.html",
    "docs/llm/harn-quickref.md",
    "docs/llm/harn-triggers-quickref.md",
];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("harn-cli lives two directories below the repo root")
        .to_path_buf()
}

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// Documentation paths referenced by shipped Rust code, e.g. `portal.html`.
fn referenced_doc_paths(root: &Path) -> BTreeSet<String> {
    const PREFIX: &str = "https://harnlang.com/";
    let mut sources = Vec::new();
    for crate_name in ["harn-cli", "harn-vm"] {
        rust_sources(
            &root.join("crates").join(crate_name).join("src"),
            &mut sources,
        );
    }

    let mut referenced = BTreeSet::new();
    for source in sources {
        // Test scaffolding is not printed to anyone, and this file quotes the
        // broken URLs it exists to prevent.
        if source.components().any(|part| part.as_os_str() == "tests") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&source) else {
            continue;
        };
        for tail in text.split(PREFIX).skip(1) {
            let url_path: String = tail
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || "._/-#".contains(*c))
                .collect();
            // A fragment addresses a heading on the page, not another page.
            let page = url_path.split('#').next().unwrap_or_default();
            if page.ends_with(".html") || page.ends_with(".md") {
                referenced.insert(page.to_string());
            }
        }
    }
    referenced
}

#[test]
fn every_documentation_link_resolves_to_a_published_page() {
    let root = repo_root();
    let referenced = referenced_doc_paths(&root);

    // A renamed crate or moved source tree would leave the scan with nothing
    // to check, and a gate that finds nothing passes for the wrong reason.
    // The missing-credentials error carries this link, so its absence means
    // the scan stopped working, not that the link went away.
    assert!(
        referenced.contains("provider-setup.html"),
        "the link scan found no known documentation URL — it is no longer \
         reading the shipped sources (found {} path(s))",
        referenced.len()
    );

    let mut broken = Vec::new();
    for path in referenced {
        if PRERENDERED_EXCEPTIONS.contains(&path.as_str()) {
            continue;
        }
        // docs/src/<page>.md is published at /<page>.html.
        let source = root
            .join("docs/src")
            .join(path.trim_end_matches(".html"))
            .with_extension("md");
        if !source.exists() {
            broken.push(format!(
                "https://harnlang.com/{path} -> {}",
                source.display()
            ));
        }
    }
    assert!(
        broken.is_empty(),
        "documentation links with no published page (docs/src/<page>.md is served \
         at /<page>.html, with no /docs/ prefix):\n  {}",
        broken.join("\n  ")
    );
}
