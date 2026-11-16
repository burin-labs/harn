use std::ops::Range;

/// The final literal command in a fixed POSIX `&&` chain. The source range is
/// observational: authorization still applies to the original invocation.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FinalOutputStage {
    /// The final command's literal argv, without preceding setup text.
    pub argv: Vec<String>,
    /// Exact byte range in the original shell command.
    pub source_range: Range<usize>,
    /// Earlier eligible commands whose executions must close stage handles.
    pub prefix_ranges: Vec<Range<usize>>,
}

impl FinalOutputStage {
    /// Original clause text in execution order, using the same measured ranges
    /// as output capture. These are source observations, never execution proof.
    pub fn clauses<'a>(&self, source: &'a str) -> Option<Vec<&'a str>> {
        if self.source_range.start >= self.source_range.end
            || self
                .prefix_ranges
                .iter()
                .any(|range| range.start >= range.end || range.end > self.source_range.start)
            || self
                .prefix_ranges
                .windows(2)
                .any(|pair| pair[0].end > pair[1].start)
        {
            return None;
        }
        self.prefix_ranges
            .iter()
            .chain(std::iter::once(&self.source_range))
            .map(|range| source.get(range.clone()))
            .collect()
    }

    /// Preserve failure context when a caller explicitly requests diagnostics
    /// but native output observation is unavailable. This grants no capture.
    pub fn diagnostic_source(&self, source: &str, diagnostics: &[String]) -> Option<String> {
        (self.clauses(source)?.len() == diagnostics.len()).then_some(())?;
        self.rewrite(source, |index, clause| {
            let message = shell_words::quote(&diagnostics[index]);
            format!(
                "{{ {clause} || {{ harn_clause_status=$?; command printf '%s\\n' {message}; exit \"$harn_clause_status\"; }}; }}"
            )
        })
    }

    /// Descriptor slots understood by every supported POSIX shell, including dash.
    pub fn descriptor_slots() -> std::ops::RangeInclusive<i32> {
        3..=9
    }

    /// Add capture redirects at measured source ranges after the process owner
    /// reserves the supplied stdout, stderr and entry slots.
    pub fn capture_source(&self, source: &str, descriptors: [i32; 3]) -> Option<String> {
        if descriptors
            .iter()
            .any(|fd| !Self::descriptor_slots().contains(fd))
            || descriptors
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != 3
        {
            return None;
        }
        let [stdout, stderr, entered] = descriptors;
        self.rewrite(source, |index, clause| {
            let event = index + 1;
            if index == self.prefix_ranges.len() {
                format!("{{ command printf '{event}\\n' >&{entered}; {clause} 1>&{stdout} 2>&{stderr} {entered}>&-; }}")
            } else {
                format!("{{ command printf '{event}\\n' >&{entered}; {clause} {stdout}>&- {stderr}>&- {entered}>&-; }}")
            }
        })
    }

    fn rewrite(
        &self,
        source: &str,
        mut render: impl FnMut(usize, &str) -> String,
    ) -> Option<String> {
        let clauses = self.clauses(source)?;
        let mut rewritten = String::with_capacity(source.len());
        let mut cursor = 0;
        let ranges = self
            .prefix_ranges
            .iter()
            .chain(std::iter::once(&self.source_range));
        for (index, (range, clause)) in ranges.zip(clauses).enumerate() {
            rewritten.push_str(source.get(cursor..range.start)?);
            rewritten.push_str(&render(index, clause));
            cursor = range.end;
        }
        rewritten.push_str(source.get(cursor..)?);
        Some(rewritten)
    }
}

/// Where command output can be attributed without evaluating shell syntax.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OutputStageInvocation {
    /// A single directly invoked program, with statically understood wrappers.
    WholeInvocation(Vec<String>),
    /// The final literal external command in an otherwise unchanged POSIX chain.
    FinalPosixStage(FinalOutputStage),
    /// Run the original invocation without granting stage attribution.
    Unmeasured,
}

/// Classify the actual process invocation with the same shell and literal-word
/// owners used by command inspection. An argv mentioning a shell is not a
/// declaration that its output belongs to a verifier.
pub fn plan_invocation_output_stage(program: &str, args: &[String]) -> OutputStageInvocation {
    match crate::shells::shell_dialect_for_id(program) {
        None => super::plan_invocation(program, args)
            .and_then(|plan| plan.literal_argv)
            .map(OutputStageInvocation::WholeInvocation)
            .unwrap_or(OutputStageInvocation::Unmeasured),
        Some(crate::shells::ShellDialect::Posix)
            if super::bounded_posix_shell_invocation(program, args) =>
        {
            args.get(1)
                .and_then(|source| plan_final_output_stage(source))
                .map(OutputStageInvocation::FinalPosixStage)
                .unwrap_or(OutputStageInvocation::Unmeasured)
        }
        _ => OutputStageInvocation::Unmeasured,
    }
}

/// Diagnostic rewriting doesn't establish bounded startup or output custody.
/// Only the known command-source position of a noninteractive POSIX invocation
/// is accepted here; unknown argument layouts retain their original source.
pub fn plan_invocation_clause_diagnostics(
    program: &str,
    args: &[String],
) -> Option<FinalOutputStage> {
    (crate::shells::shell_dialect_for_id(program) == Some(crate::shells::ShellDialect::Posix)
        && args.first().map(String::as_str) == Some("-c"))
    .then_some(())?;
    plan_final_output_stage(args.get(1)?)
}

/// Locate a final external command without executing or reconstructing its
/// preceding setup. Unsupported control flow has no attributable stage.
pub fn plan_final_output_stage(source: &str) -> Option<FinalOutputStage> {
    let shape =
        crate::orchestration::shell_command_shape(crate::shells::ShellDialect::Posix, source);
    let segments = shape.segments?;
    let mut prefix_ranges = Vec::new();
    for (index, segment) in segments.iter().enumerate() {
        if segment.operator != if index == 0 { "" } else { "&&" } || segment.stages.len() != 1 {
            return None;
        }
        let stage = &segment.stages[0];
        if index + 1 < segments.len() {
            let command = super::plan_posix_command_inner(&stage.text, true)?;
            let argv = command.argv?;
            let program = argv.first()?;
            // These setup builtins preserve the shell and its descriptors.
            // Other builtins can terminate it, install handlers or redirect
            // later commands independently of this parsed chain.
            if super::shell_builtin(program)
                && !matches!(
                    program.as_str(),
                    "cd" | "export" | "test" | "[" | "printf" | "echo" | ":"
                )
            {
                return None;
            }
            let end = stage.start_byte.checked_add(stage.text.len())?;
            (source.get(stage.start_byte..end)? == stage.text).then_some(())?;
            prefix_ranges.push(stage.start_byte..end);
        }
    }
    let stage = &segments.last()?.stages[0];
    let planned = super::plan_posix_command(&stage.text)?;
    let argv = planned.argv?;
    let start = stage.start_byte;
    let end = start.checked_add(stage.text.len())?;
    (source.get(start..end)? == stage.text).then_some(FinalOutputStage {
        argv,
        source_range: start..end,
        prefix_ranges,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn final_stage_retains_original_setup_and_literal_arguments() {
        let source = "  printf '1 passed && still setup\\n' && cd 'test project' && prepare-test-data --local && pytest 'case with spaces.py'";
        let stage = plan_final_output_stage(source).expect("ordinary setup and final verifier");
        assert_eq!(stage.argv, ["pytest", "case with spaces.py"]);
        assert_eq!(
            source.get(stage.source_range),
            Some("pytest 'case with spaces.py'")
        );
    }

    #[test]
    fn final_stage_does_not_promote_quoted_verifier_text_to_program_identity() {
        let stage = plan_final_output_stage("printf ready && python -c 'pytest in quoted text'")
            .expect("one literal Python command");
        assert_eq!(stage.argv[0], "python");
        assert_eq!(stage.argv[1], "-c");
    }

    #[test]
    fn diagnostics_and_capture_share_exact_original_clause_ranges() {
        let source =
            "  printf 'setup && canary' && cd 'test project' && pytest 'case with spaces.py'";
        let plan = plan_final_output_stage(source).unwrap();
        assert_eq!(
            plan.clauses(source).unwrap(),
            [
                "printf 'setup && canary'",
                "cd 'test project'",
                "pytest 'case with spaces.py'"
            ]
        );
        let diagnostics = ["setup", "directory", "verifier"].map(str::to_owned);
        let diagnostic = plan.diagnostic_source(source, &diagnostics).unwrap();
        assert!(diagnostic.starts_with("  { printf 'setup && canary' || {"));
        assert!(diagnostic.contains("verifier"));
        assert_eq!(diagnostic.matches("command printf '%s\\n'").count(), 3);
        assert!(!diagnostic.contains("1>&"));
    }

    #[test]
    fn literal_setup_environment_and_file_guards_share_the_same_clause_plan() {
        let source =
            "export CHECK_MODE=fixture && test -f case.py && [ -f case.py ] && pytest case.py";
        let plan = plan_final_output_stage(source).unwrap();
        assert_eq!(plan.clauses(source).unwrap().len(), 4);
        let capture = plan.capture_source(source, [3, 4, 5]).unwrap();
        assert!(capture.contains("export CHECK_MODE=fixture 3>&- 4>&- 5>&-"));
        assert!(capture.contains("pytest case.py 1>&3 2>&4 5>&-"));
        assert_eq!(capture.matches("command printf '").count(), 4);
    }

    #[test]
    fn invalid_clause_ranges_refuse_both_diagnostics_and_capture() {
        let source = "printf ready && pytest case.py";
        let measured = plan_final_output_stage(source).unwrap();
        for range in [
            0..0,
            0..source.len(),
            source.len()..1,
            source.len()..source.len() + 1,
        ] {
            let mut invalid = measured.clone();
            invalid.prefix_ranges = vec![range];
            assert!(invalid.clauses(source).is_none());
            assert!(invalid
                .diagnostic_source(source, &["failure".into()])
                .is_none());
            assert!(invalid.capture_source(source, [3, 4, 5]).is_none());
        }
    }

    #[test]
    fn uncertain_control_flow_and_output_ownership_are_unmeasured() {
        for source in [
            "printf ready; pytest test.py",
            "printf ready || pytest test.py",
            "printf ready | pytest test.py",
            "printf ready & pytest test.py",
            "(printf ready) && pytest test.py",
            "exit 0 && pytest test.py",
            "exec printf ready && pytest test.py",
            "trap 'printf fake' EXIT && pytest test.py",
            "source setup.sh && pytest test.py",
            "export CHECK=$(printf expanded) && pytest test.py",
            "[ -f $(printf case.py) ] && pytest test.py",
            "[ -f case.py > other.txt ] && pytest test.py",
            "exec 2>&1 && pytest test.py",
            "printf ready && $VERIFIER test.py",
            "printf ready && pytest $(printf test.py)",
            "printf ready && pytest test.py > report.txt",
            "printf ready && echo 'pytest test.py'",
        ] {
            assert!(plan_final_output_stage(source).is_none(), "{source}");
        }
    }
}
