//! Bounded evidence over the exact source images a deterministic edit compared.

use serde::Serialize;

use crate::ast::{api, Language, ParserHealth, ParserOperation};
use harn_vm::text_diff::{compute_line_diff, LineChangeKind, LineDiffOptions};
use harn_vm::VmValue;

const SITE_LIMIT: usize = 12;
const EXCERPT_LIMIT: usize = 180;

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum SyntaxStatus {
    Passed,
    Failed,
    Unavailable,
}

#[derive(Debug, Serialize)]
struct SyntaxCheck {
    status: SyntaxStatus,
    engine: &'static str,
    detail: String,
}

#[derive(Debug, Serialize)]
struct ChangedSite {
    before_line: usize,
    after_line: usize,
    before: String,
    after: String,
}

#[derive(Debug, Serialize)]
struct FileProof {
    path: String,
    site_kind: &'static str,
    site_count: usize,
    sites: Vec<ChangedSite>,
    omitted_sites: usize,
    syntax: SyntaxCheck,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct PreservedSite {
    pub path: String,
    pub line: usize,
    pub text: String,
    pub reason: String,
}

#[derive(Debug, Serialize)]
struct Preserved {
    status: &'static str,
    site_count: Option<usize>,
    sites: Vec<PreservedSite>,
    omitted_sites: usize,
}

#[derive(Debug, Serialize)]
struct Semantics {
    status: &'static str,
    engine: &'static str,
    elapsed_ms: Option<u64>,
    detail: &'static str,
}

#[derive(Debug, Serialize)]
pub(crate) struct EditProof {
    schema: &'static str,
    files: Vec<FileProof>,
    file_count: usize,
    site_count: usize,
    preserved: Preserved,
    semantics: Semantics,
}

pub(crate) struct EditRegion<'a> {
    pub start: usize,
    pub end: usize,
    pub replacement: &'a str,
}

impl EditProof {
    pub(crate) fn new() -> Self {
        Self {
            schema: "harn.edit_proof.v1",
            files: Vec::new(),
            file_count: 0,
            site_count: 0,
            preserved: Preserved {
                status: "unavailable",
                site_count: None,
                sites: Vec::new(),
                omitted_sites: 0,
            },
            semantics: Semantics {
                status: "not_run",
                engine: "",
                elapsed_ms: None,
                detail: "",
            },
        }
    }

    pub(crate) fn add_file(
        &mut self,
        path: &str,
        before: &str,
        after: &str,
        language: Option<Language>,
        edits: Option<Vec<EditRegion<'_>>>,
    ) {
        let remaining =
            SITE_LIMIT.saturating_sub(self.files.iter().map(|file| file.sites.len()).sum());
        let (site_kind, mut sites) = if let Some(mut edits) = edits {
            edits.sort_by_key(|edit| edit.start);
            let mut shift = 0isize;
            let sites = edits
                .into_iter()
                .filter_map(|edit| {
                    let old = before.get(edit.start..edit.end)?;
                    let after_start = edit.start.checked_add_signed(shift)?;
                    shift += edit.replacement.len() as isize - (edit.end - edit.start) as isize;
                    (old != edit.replacement).then(|| ChangedSite {
                        before_line: line_of(before, edit.start),
                        after_line: line_of(after, after_start),
                        before: excerpt(before, edit.start),
                        after: excerpt(after, after_start),
                    })
                })
                .collect::<Vec<_>>();
            ("edits", sites)
        } else {
            ("line_regions", changed_regions(before, after))
        };
        let site_count = sites.len();
        sites.truncate(remaining);
        let omitted_sites = site_count - sites.len();
        self.files.push(FileProof {
            path: path.to_owned(),
            site_kind,
            site_count,
            sites,
            omitted_sites,
            syntax: syntax(after, language),
        });
        self.file_count += 1;
        self.site_count += site_count;
    }

    pub(crate) fn preserved(&mut self, sites: Vec<PreservedSite>) {
        let site_count = sites.len();
        let remaining =
            SITE_LIMIT.saturating_sub(self.files.iter().map(|file| file.sites.len()).sum());
        let sites = sites
            .into_iter()
            .take(remaining)
            .map(|mut site| {
                site.text = bounded(&site.text);
                site
            })
            .collect::<Vec<_>>();
        self.preserved = Preserved {
            status: "checked",
            site_count: Some(site_count),
            omitted_sites: site_count - sites.len(),
            sites,
        };
    }

    pub(crate) fn value(&self) -> VmValue {
        harn_vm::bridge::json_result_to_vm_value(
            &serde_json::to_value(self).expect("edit proof serializes"),
        )
    }
}

fn bounded(text: &str) -> String {
    text.chars().take(EXCERPT_LIMIT).collect()
}

fn line_of(source: &str, offset: usize) -> usize {
    source[..offset.min(source.len())]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count()
        + 1
}

fn excerpt(source: &str, offset: usize) -> String {
    let offset = offset.min(source.len());
    let start = source[..offset].rfind('\n').map_or(0, |at| at + 1);
    bounded(source[start..].lines().next().unwrap_or(""))
}

fn changed_regions(before: &str, after: &str) -> Vec<ChangedSite> {
    let diff = compute_line_diff(
        before,
        after,
        LineDiffOptions {
            context: 0,
            include_body: false,
            include_changes: true,
        },
    );
    let mut sites = Vec::new();
    let mut current: Option<ChangedSite> = None;
    for change in diff.changes {
        if change.kind == LineChangeKind::Equal {
            if let Some(site) = current.take() {
                sites.push(site);
            }
            continue;
        }
        let site = current.get_or_insert_with(|| ChangedSite {
            before_line: change.old_line,
            after_line: change.new_line,
            before: String::new(),
            after: String::new(),
        });
        let text = if change.kind == LineChangeKind::Delete {
            &mut site.before
        } else {
            &mut site.after
        };
        if text.chars().count() < EXCERPT_LIMIT {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&change.line);
            *text = bounded(text);
        }
    }
    if let Some(site) = current {
        sites.push(site);
    }
    sites
}

fn syntax(source: &str, language: Option<Language>) -> SyntaxCheck {
    let Some(language) = language else {
        return SyntaxCheck {
            status: SyntaxStatus::Unavailable,
            engine: "",
            detail: "language unavailable".into(),
        };
    };
    syntax_result(
        language,
        api::parse_tree(source, language)
            .map(|tree| tree.root_node().has_error())
            .map_err(|error| error.to_string()),
    )
}

fn syntax_result(language: Language, result: Result<bool, String>) -> SyntaxCheck {
    match result {
        Err(detail) => SyntaxCheck {
            status: SyntaxStatus::Unavailable,
            engine: "tree_sitter",
            detail,
        },
        Ok(true) => SyntaxCheck {
            status: SyntaxStatus::Failed,
            engine: "tree_sitter",
            detail: "source contains parse errors".into(),
        },
        Ok(false)
            if ParserHealth::observed(language, ParserOperation::SafeEdit, false).is_verified() =>
        {
            SyntaxCheck {
                status: SyntaxStatus::Passed,
                engine: "tree_sitter",
                detail: String::new(),
            }
        }
        Ok(false) => SyntaxCheck {
            status: SyntaxStatus::Unavailable,
            engine: "tree_sitter",
            detail: "parser fitness is unverified".into(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn syntax_never_treats_an_unavailable_parser_as_clean() {
        assert!(matches!(
            syntax_result(Language::Java, Err("grammar unavailable".into())).status,
            SyntaxStatus::Unavailable
        ));
        assert!(matches!(
            syntax("class A {", Some(Language::Java)).status,
            SyntaxStatus::Failed
        ));
        assert!(matches!(
            syntax("class A {}", Some(Language::Java)).status,
            SyntaxStatus::Passed
        ));
    }

    #[test]
    fn counts_all_regions_but_bounds_excerpts() {
        let mut proof = EditProof::new();
        for index in 0..14 {
            proof.add_file(
                &format!("{index}.java"),
                "class A {}",
                "class B {}",
                Some(Language::Java),
                None,
            );
        }
        assert_eq!(proof.site_count, 14);
        assert_eq!(
            proof
                .files
                .iter()
                .map(|file| file.sites.len())
                .sum::<usize>(),
            12
        );
        assert_eq!(
            proof
                .files
                .iter()
                .map(|file| file.omitted_sites)
                .sum::<usize>(),
            2
        );
        assert_eq!(proof.file_count, 14);
        assert_eq!(proof.preserved.site_count, None);
        proof.preserved(Vec::new());
        assert_eq!(proof.preserved.site_count, Some(0));
    }

    #[test]
    fn exact_edits_adjust_after_line_positions() {
        let mut proof = EditProof::new();
        proof.add_file(
            "a.java",
            "one\ntwo\n",
            "first\nextra\nsecond\n",
            None,
            Some(vec![
                EditRegion {
                    start: 0,
                    end: 3,
                    replacement: "first\nextra",
                },
                EditRegion {
                    start: 4,
                    end: 7,
                    replacement: "second",
                },
            ]),
        );
        assert_eq!(proof.files[0].sites[1].before_line, 2);
        assert_eq!(proof.files[0].sites[1].after_line, 3);
        assert_eq!(proof.files[0].sites[1].after, "second");
    }
}
