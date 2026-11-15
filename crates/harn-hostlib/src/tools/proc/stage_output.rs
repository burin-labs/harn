use std::io;
#[cfg(unix)]
use std::sync::Arc;

use harn_vm::session_inputs::OutputFilter;
use serde::Serialize;

use crate::process::OutputCapture;

#[cfg(unix)]
mod observation;

/// A capture observation, not an attestation that an executable ran tests.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum StageOutput {
    Unmeasured {
        reason: &'static str,
        #[serde(skip_serializing_if = "Option::is_none")]
        failed_clause: Option<FailedClause>,
    },
    Captured {
        argv: Vec<String>,
        stdout: String,
        stderr: String,
        stdout_truncated: bool,
        stderr_truncated: bool,
        output_path: String,
        stdout_path: String,
        stderr_path: String,
        line_count: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        failed_clause: Option<FailedClause>,
    },
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct FailedClause {
    index: usize,
    total: usize,
    source: String,
}

impl StageOutput {
    pub(crate) fn unmeasured(reason: &'static str) -> Self {
        Self::Unmeasured {
            reason,
            failed_clause: None,
        }
    }
}

pub(crate) struct StageOutputBytes<'a> {
    #[cfg(unix)]
    pub(crate) relay_complete: bool,
    pub(crate) stdout: &'a [u8],
    pub(crate) stderr: &'a [u8],
    #[cfg(unix)]
    pub(crate) stage_stdout: &'a [u8],
    #[cfg(unix)]
    pub(crate) stage_stderr: &'a [u8],
    #[cfg(unix)]
    pub(crate) observation: &'a [u8],
}

pub(crate) struct StageReaders {
    pub(crate) stdout: Box<dyn io::Read + Send>,
    pub(crate) stderr: Box<dyn io::Read + Send>,
    pub(crate) observation: Box<dyn io::Read + Send>,
}

#[derive(Debug)]
pub(crate) enum StageCapture {
    Unmeasured(&'static str),
    WholeInvocation(Vec<String>),
    #[cfg(unix)]
    Shell {
        argv: Vec<String>,
        readers: Option<(
            std::os::unix::net::UnixStream,
            std::os::unix::net::UnixStream,
            std::os::unix::net::UnixStream,
        )>,
        clauses: Vec<String>,
    },
}

impl StageCapture {
    pub(crate) fn for_spawn(
        spec: &mut crate::process::SpawnSpec,
        capture: &super::CaptureConfig,
    ) -> Result<Option<Self>, crate::error::HostlibError> {
        if !capture.final_stage {
            return Ok(None);
        }
        let (stage, output) = if capture.transport != super::CaptureTransport::Pipe {
            (Self::Unmeasured("capture_transport_unmeasured"), None)
        } else {
            Self::prepare(&spec.program, &spec.args).map_err(|error| {
                crate::error::HostlibError::Backend {
                    builtin: spec.builtin,
                    message: format!("prepare final-stage capture: {error}"),
                }
            })?
        };
        if let Some(output) = output {
            #[cfg(unix)]
            let output = match output {
                OutputCapture::PipeWithStage {
                    stdout,
                    stderr,
                    entered,
                    plan,
                    ..
                } => {
                    validate_diagnostics(
                        spec.builtin,
                        &plan,
                        &spec.args[1],
                        capture.clause_diagnostics.as_deref(),
                    )?;
                    OutputCapture::PipeWithStage {
                        stdout,
                        stderr,
                        entered,
                        plan,
                        clause_diagnostics: capture.clause_diagnostics.clone(),
                    }
                }
                other => other,
            };
            spec.output_capture = output;
        } else if let Some(diagnostics) = capture.clause_diagnostics.as_deref() {
            if let Some((source_arg, plan)) = diagnostic_plan(spec, capture.shell_source_arg) {
                validate_diagnostics(
                    spec.builtin,
                    &plan,
                    &spec.args[source_arg],
                    Some(diagnostics),
                )?;
                if let Some(rewritten) = plan.diagnostic_source(&spec.args[source_arg], diagnostics)
                {
                    spec.args[source_arg] = rewritten;
                }
            }
        }
        Ok(Some(stage))
    }

    /// Rewrite only an observed final stage; callers authorize the original
    /// invocation before handing this capture to the existing process owner.
    fn prepare(program: &str, args: &[String]) -> io::Result<(Self, Option<OutputCapture>)> {
        let stage = match harn_vm::shells::plan_invocation_output_stage(program, args) {
            harn_vm::shells::OutputStageInvocation::WholeInvocation(argv) => {
                return Ok((Self::WholeInvocation(argv), None));
            }
            harn_vm::shells::OutputStageInvocation::Unmeasured => {
                return Ok((Self::Unmeasured("shell_invocation_unmeasured"), None));
            }
            harn_vm::shells::OutputStageInvocation::FinalPosixStage(stage) => stage,
        };
        #[cfg(not(unix))]
        {
            let _ = stage;
            return Ok((Self::Unmeasured("descriptor_transport_unmeasured"), None));
        }
        #[cfg(unix)]
        {
            use std::os::fd::OwnedFd;
            // This backend closes unlisted descriptors in its namespace
            // helper. Leave the original invocation intact until it owns a
            // stage-output descriptor projection too.
            if harn_vm::process_sandbox::active_backend_mechanism()
                == harn_vm::process_sandbox::SandboxMechanism::LinuxBubblewrap
            {
                return Ok((
                    Self::Unmeasured("namespace_descriptor_transport_unmeasured"),
                    None,
                ));
            }
            let (stdout_reader, stdout_writer) = std::os::unix::net::UnixStream::pair()?;
            let (stderr_reader, stderr_writer) = std::os::unix::net::UnixStream::pair()?;
            let (observation_reader, observation_writer) = std::os::unix::net::UnixStream::pair()?;
            let stdout = Arc::new(std::fs::File::from(OwnedFd::from(stdout_writer)));
            let stderr = Arc::new(std::fs::File::from(OwnedFd::from(stderr_writer)));
            let entered = Arc::new(std::fs::File::from(OwnedFd::from(observation_writer)));
            let clauses = stage
                .clauses(&args[1])
                .ok_or_else(|| io::Error::other("invalid command output source plan"))?
                .into_iter()
                .map(str::to_owned)
                .collect();
            Ok((
                Self::Shell {
                    argv: stage.argv.clone(),
                    readers: Some((stdout_reader, stderr_reader, observation_reader)),
                    clauses,
                },
                Some(OutputCapture::PipeWithStage {
                    stdout,
                    stderr,
                    entered,
                    plan: stage,
                    clause_diagnostics: None,
                }),
            ))
        }
    }

    pub(crate) fn take_readers(&mut self) -> Option<StageReaders> {
        match self {
            #[cfg(unix)]
            Self::Shell { readers, .. } => {
                readers
                    .take()
                    .map(|(stdout, stderr, observation)| StageReaders {
                        stdout: Box::new(stdout),
                        stderr: Box::new(stderr),
                        observation: Box::new(observation),
                    })
            }
            _ => None,
        }
    }

    pub(crate) fn finish(
        &self,
        bytes: StageOutputBytes<'_>,
        exit_code: Option<i32>,
        capture: &super::CaptureConfig,
        filter: &OutputFilter,
    ) -> Result<StageOutput, crate::error::HostlibError> {
        let Some(exit_code) = exit_code else {
            return Ok(StageOutput::unmeasured("command_not_completed"));
        };
        let mut failed_clause = None;
        let (argv, stdout, stderr) = match self {
            Self::Unmeasured(reason) => return Ok(StageOutput::unmeasured(reason)),
            Self::WholeInvocation(argv) => (argv.clone(), bytes.stdout, bytes.stderr),
            #[cfg(unix)]
            Self::Shell { argv, clauses, .. } => {
                if !bytes.relay_complete {
                    return Ok(StageOutput::unmeasured("stage_relay_unconfirmed"));
                }
                let observed =
                    observation::read(&mut io::Cursor::new(bytes.observation), clauses.len())
                        .ok()
                        .flatten();
                if let Some(index) = observed.filter(|_| exit_code != 0) {
                    let source = &clauses[index - 1];
                    failed_clause = Some(FailedClause {
                        index,
                        total: clauses.len(),
                        source: String::from_utf8_lossy(&filter.redact_bytes(source.as_bytes()))
                            .into_owned(),
                    });
                }
                if observed != Some(clauses.len()) {
                    return Ok(StageOutput::Unmeasured {
                        reason: "final_stage_unobserved",
                        failed_clause,
                    });
                }
                (argv.clone(), bytes.stage_stdout, bytes.stage_stderr)
            }
        };
        let argv = argv
            .into_iter()
            .map(|word| String::from_utf8_lossy(&filter.redact_bytes(word.as_bytes())).into_owned())
            .collect();
        let artifacts = super::persist_artifacts(&super::next_command_id(), stdout, stderr, None)?;
        let inline = super::inline_output(stdout, stderr, capture.clone());
        Ok(StageOutput::Captured {
            argv,
            stdout: inline.stdout,
            stderr: inline.stderr,
            stdout_truncated: inline.stdout_truncated,
            stderr_truncated: inline.stderr_truncated,
            output_path: artifacts.output_path.to_string_lossy().into_owned(),
            stdout_path: artifacts.stdout_path.to_string_lossy().into_owned(),
            stderr_path: artifacts.stderr_path.to_string_lossy().into_owned(),
            line_count: artifacts.line_count,
            failed_clause,
        })
    }
}

fn diagnostic_plan(
    spec: &crate::process::SpawnSpec,
    source_arg: Option<usize>,
) -> Option<(usize, harn_vm::shells::FinalOutputStage)> {
    if let Some(index) = source_arg {
        return harn_vm::shells::plan_final_output_stage(spec.args.get(index)?)
            .map(|plan| (index, plan));
    }
    harn_vm::shells::plan_invocation_clause_diagnostics(&spec.program, &spec.args)
        .map(|plan| (1, plan))
}

fn validate_diagnostics(
    builtin: &'static str,
    plan: &harn_vm::shells::FinalOutputStage,
    source: &str,
    diagnostics: Option<&[String]>,
) -> Result<(), crate::error::HostlibError> {
    if let Some(diagnostics) = diagnostics {
        if plan
            .clauses(source)
            .is_none_or(|clauses| clauses.len() != diagnostics.len())
        {
            return Err(crate::error::HostlibError::InvalidParameter {
                builtin,
                param: "capture.clause_diagnostics",
                message: "expected one diagnostic string per measured command clause".into(),
            });
        }
    }
    Ok(())
}
