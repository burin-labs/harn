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
    /// Descriptor slots understood by every supported POSIX shell, including dash.
    pub fn descriptor_slots() -> std::ops::RangeInclusive<i32> {
        3..=9
    }

    /// Add capture redirects at measured source ranges after the process owner
    /// reserves the supplied stdout, stderr and entry slots.
    pub fn capture_source(&self, source: &str, descriptors: [i32; 3]) -> Option<String> {
        if self.source_range.start >= self.source_range.end
            || descriptors
                .iter()
                .any(|fd| !Self::descriptor_slots().contains(fd))
            || descriptors
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != 3
            || self.prefix_ranges.iter().any(|range| {
                range.start >= range.end
                    || range.end > self.source_range.start
                    || source.get(range.clone()).is_none()
            })
            || self
                .prefix_ranges
                .windows(2)
                .any(|pair| pair[0].end > pair[1].start)
        {
            return None;
        }
        let original = source.get(self.source_range.clone())?;
        let [stdout, stderr, entered] = descriptors;
        let replacement =
            format!("{{ command printf 1 >&{entered} && {original}; }} 1>&{stdout} 2>&{stderr}");
        let mut rewritten = source.to_owned();
        rewritten.replace_range(self.source_range.clone(), &replacement);
        let close = format!(" {stdout}>&- {stderr}>&- {entered}>&-");
        for prefix in self.prefix_ranges.iter().rev() {
            rewritten.insert_str(prefix.end, &close);
        }
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

/// Locate a final external command without executing or reconstructing its
/// preceding setup. Unsupported control flow has no attributable stage.
pub fn plan_final_output_stage(source: &str) -> Option<FinalOutputStage> {
    let shape =
        crate::orchestration::shell_command_shape(crate::shells::ShellDialect::Posix, source);
    let segments = shape.segments?;
    let mut prefix_ranges = Vec::new();
    for (index, segment) in segments.iter().enumerate() {
        if segment.operator != if index == 0 { "" } else { "&&" }
            || segment.stages.len() != 1
            || segment.stages[0].kind != "command"
        {
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
                && !matches!(program.as_str(), "cd" | "printf" | "echo" | ":")
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
