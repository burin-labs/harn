use std::io;
#[cfg(unix)]
use std::sync::Arc;

use harn_vm::session_inputs::OutputFilter;
use serde::Serialize;

use crate::process::OutputCapture;

/// A capture observation, not an attestation that an executable ran tests.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum StageOutput {
    Unmeasured {
        reason: &'static str,
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
    },
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
}

pub(crate) struct StageReaders {
    pub(crate) stdout: Box<dyn io::Read + Send>,
    pub(crate) stderr: Box<dyn io::Read + Send>,
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
        )>,
        entered: Arc<std::fs::File>,
    },
}

impl StageCapture {
    pub(crate) fn for_spawn(
        spec: &mut crate::process::SpawnSpec,
        capture: super::CaptureConfig,
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
            spec.output_capture = output;
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
            let stdout = Arc::new(std::fs::File::from(OwnedFd::from(stdout_writer)));
            let stderr = Arc::new(std::fs::File::from(OwnedFd::from(stderr_writer)));
            let entered = Arc::new(tempfile::tempfile()?);
            Ok((
                Self::Shell {
                    argv: stage.argv.clone(),
                    readers: Some((stdout_reader, stderr_reader)),
                    entered: entered.clone(),
                },
                Some(OutputCapture::PipeWithStage {
                    stdout,
                    stderr,
                    entered,
                    plan: stage,
                }),
            ))
        }
    }

    pub(crate) fn take_readers(&mut self) -> Option<StageReaders> {
        match self {
            #[cfg(unix)]
            Self::Shell { readers, .. } => readers.take().map(|(stdout, stderr)| StageReaders {
                stdout: Box::new(stdout),
                stderr: Box::new(stderr),
            }),
            _ => None,
        }
    }

    pub(crate) fn finish(
        &self,
        bytes: StageOutputBytes<'_>,
        completed: bool,
        capture: super::CaptureConfig,
        filter: &OutputFilter,
    ) -> Result<StageOutput, crate::error::HostlibError> {
        if !completed {
            return Ok(StageOutput::Unmeasured {
                reason: "command_not_completed",
            });
        }
        let (argv, stdout, stderr) = match self {
            Self::Unmeasured(reason) => return Ok(StageOutput::Unmeasured { reason }),
            Self::WholeInvocation(argv) => (argv.clone(), bytes.stdout, bytes.stderr),
            #[cfg(unix)]
            Self::Shell { argv, entered, .. } => {
                if !bytes.relay_complete {
                    return Ok(StageOutput::Unmeasured {
                        reason: "stage_relay_unconfirmed",
                    });
                }
                if !entry_recorded(entered) {
                    return Ok(StageOutput::Unmeasured {
                        reason: "final_stage_unobserved",
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
        let inline = super::inline_output(stdout, stderr, capture);
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
        })
    }
}

#[cfg(unix)]
fn entry_recorded(file: &std::fs::File) -> bool {
    use std::os::unix::fs::FileExt;
    if !file.metadata().is_ok_and(|metadata| metadata.len() == 1) {
        return false;
    }
    let mut byte = [0];
    file.read_exact_at(&mut byte, 0).is_ok() && byte == *b"1"
}
