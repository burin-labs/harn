//! Bounded evidence over the exact source images a deterministic edit compared.

use serde::Serialize;

use crate::ast::{api, Language, ParserHealth, ParserOperation};
use harn_vm::text_diff::{compute_line_diff_with_endings, LineChangeKind, LineDiffOptions};
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
        let (site_kind, mut sites) = match edits.and_then(|edits| exact_sites(before, after, edits))
        {
            Some(sites) => ("edits", sites),
            None => ("line_regions", changed_regions(before, after)),
        };
        let site_count = sites.len();
        sites.retain(|site| site.before != site.after);
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
        let changed_budget = SITE_LIMIT - site_count.min(2);
        let mut used = 0;
        for file in &mut self.files {
            let keep = file.sites.len().min(changed_budget.saturating_sub(used));
            file.omitted_sites += file.sites.len() - keep;
            file.sites.truncate(keep);
            used += keep;
        }
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
    source
        .bytes()
        .take(offset)
        .filter(|byte| *byte == b'\n')
        .count()
        + 1
}

fn excerpt(source: &str, offset: usize, end: usize) -> Option<String> {
    let start = source.get(..offset)?.rfind('\n').map_or(0, |at| at + 1);
    let end = if end > offset && source.get(..end)?.ends_with('\n') {
        end
    } else {
        source
            .get(end..)?
            .find('\n')
            .map_or(source.len(), |at| end + at)
    };
    Some(bounded(source.get(start..end)?))
}

fn exact_sites(
    before: &str,
    after: &str,
    mut edits: Vec<EditRegion<'_>>,
) -> Option<Vec<ChangedSite>> {
    edits.sort_by_key(|edit| edit.start);
    let mut shift = 0isize;
    let mut sites = Vec::new();
    for edit in edits {
        let old = before.get(edit.start..edit.end)?;
        let after_start = edit.start.checked_add_signed(shift)?;
        let after_end = after_start.checked_add(edit.replacement.len())?;
        if after.get(after_start..after_end)? != edit.replacement {
            return None;
        }
        shift += edit.replacement.len() as isize - (edit.end - edit.start) as isize;
        if old != edit.replacement {
            sites.push(ChangedSite {
                before_line: line_of(before, edit.start),
                after_line: line_of(after, after_start),
                before: excerpt(before, edit.start, edit.end)?,
                after: excerpt(after, after_start, after_end)?,
            });
        }
    }
    Some(sites)
}

fn changed_regions(before: &str, after: &str) -> Vec<ChangedSite> {
    let diff = compute_line_diff_with_endings(
        before,
        after,
        LineDiffOptions {
            context: 0,
            include_body: false,
            include_changes: true,
        },
    );
    let mut sites = Vec::new();
    let mut current = None;
    let mut before_offset = 0;
    let mut after_offset = 0;
    let site = |(before_start, after_start), before_end, after_end| ChangedSite {
        before_line: line_of(before, before_start),
        after_line: line_of(after, after_start),
        before: excerpt(before, before_start, before_end)
            .expect("shared diff before span must be valid UTF8"),
        after: excerpt(after, after_start, after_end)
            .expect("shared diff after span must be valid UTF8"),
    };
    for change in diff.changes {
        if change.kind == LineChangeKind::Equal {
            if let Some(start) = current.take() {
                sites.push(site(start, before_offset, after_offset));
            }
        } else {
            current.get_or_insert((before_offset, after_offset));
        }
        let bytes = change.line.len();
        match change.kind {
            LineChangeKind::Equal => {
                before_offset += bytes;
                after_offset += bytes;
            }
            LineChangeKind::Delete => before_offset += bytes,
            LineChangeKind::Insert => after_offset += bytes,
        }
    }
    if let Some(start) = current {
        sites.push(site(start, before_offset, after_offset));
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
    fn bounded_prefixes_that_hide_the_edit_count_as_omitted() {
        let before = format!(
            "class A {{ String pad = \"{}\"; void oldName() {{}} }}",
            "x".repeat(240)
        );
        let after = before.replace("oldName", "newName");
        let start = before.find("oldName").unwrap();
        for edits in [
            None,
            Some(vec![EditRegion {
                start,
                end: start + "oldName".len(),
                replacement: "newName",
            }]),
        ] {
            let mut proof = EditProof::new();
            proof.add_file("A.java", &before, &after, Some(Language::Java), edits);
            assert_eq!(proof.site_count, 1);
            assert_eq!(proof.files[0].site_count, 1);
            assert!(proof.files[0].sites.is_empty());
            assert_eq!(proof.files[0].omitted_sites, 1);
        }
    }

    #[test]
    fn line_regions_keep_changed_line_terminators() {
        let inserted = changed_regions("a\nb\n", "a\n\nb\n");
        assert_eq!(inserted.len(), 1);
        assert_eq!(inserted[0].after_line, 2);
        assert_eq!(inserted[0].after, "\n");
        let ending = changed_regions("a\r\n", "a\n");
        assert_eq!(ending[0].before, "a\r\n");
        assert_eq!(ending[0].after, "a\n");
        let final_newline = changed_regions("a", "a\n");
        assert_eq!(final_newline[0].before, "a");
        assert_eq!(final_newline[0].after, "a\n");
        let carriage_return = changed_regions("a\rb\r", "a\rB\r");
        assert_eq!(carriage_return[0].before_line, 1);
        assert_eq!(carriage_return[0].before, "a\rb\r");
        assert_eq!(carriage_return[0].after, "a\rB\r");
    }

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
        proof.preserved(
            (0..3)
                .map(|index| PreservedSite {
                    path: "names.java".into(),
                    line: index + 1,
                    text: "name".into(),
                    reason: "resolved elsewhere".into(),
                })
                .collect(),
        );
        assert_eq!(proof.preserved.site_count, Some(3));
        assert_eq!(proof.preserved.sites.len(), 2);
        assert_eq!(
            proof
                .files
                .iter()
                .map(|file| file.sites.len())
                .sum::<usize>(),
            10
        );
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

    #[test]
    fn inserted_helper_excerpt_includes_code_after_its_leading_newline() {
        let mut proof = EditProof::new();
        proof.add_file(
            "a.ts",
            "const value = 1;\n",
            "const value = 1;\n\nfunction helper() { return value; }\n",
            None,
            Some(vec![EditRegion {
                start: 17,
                end: 17,
                replacement: "\nfunction helper() { return value; }\n",
            }]),
        );
        assert_eq!(proof.files[0].sites[0].after_line, 2);
        assert!(proof.files[0].sites[0].after.contains("function helper()"));
    }

    #[test]
    fn invalid_utf8_edit_boundaries_fall_back_to_measured_line_regions() {
        let mut proof = EditProof::new();
        proof.add_file(
            "a.ts",
            "é",
            "ø",
            None,
            Some(vec![EditRegion {
                start: 1,
                end: 2,
                replacement: "ø",
            }]),
        );
        assert_eq!(proof.files[0].site_kind, "line_regions");
        assert_eq!(proof.site_count, 1);
        assert_eq!(proof.files[0].sites[0].before, "é");
        assert_eq!(proof.files[0].sites[0].after, "ø");
    }
}
