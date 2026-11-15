use super::*;

impl PreparedCommand {
    /// Admit the private startup request without claiming any descriptor twice.
    ///
    /// # Safety
    /// Every accepted descriptor must have been transferred exclusively to this
    /// guardian by the host launch. Shape/open checks cannot establish custody.
    pub(super) unsafe fn from_inherited_request(
        mut request: GuardianCommandRequest,
    ) -> io::Result<Self> {
        let mut inherited_descriptors = {
            let numbers = request.stage_output_descriptors.clone();
            if !numbers.is_empty() && numbers.len() != 3 {
                return Err(io::Error::other("invalid stage output descriptor roles"));
            }
            #[cfg(target_os = "linux")]
            let numbers = {
                let mut numbers = numbers;
                numbers.extend(request.pinned_verifier_descriptors.iter().copied());
                if let Some(GuardianConfinement::Bubblewrap { descriptors }) = &request.confinement
                {
                    numbers.extend(descriptors.iter().copied());
                }
                numbers
            };
            let reserved = request.cleanup_receipt_fd.into_iter().collect::<Vec<_>>();
            #[cfg(target_os = "linux")]
            let reserved = {
                let mut reserved = reserved;
                reserved.extend(request.cancel_request_fd);
                if matches!(
                    &request.confinement,
                    Some(
                        GuardianConfinement::BeforeExec { ruleset: true, .. }
                            | GuardianConfinement::AfterNamespace { ruleset: true }
                    )
                ) {
                    reserved.push(RULESET_FD);
                }
                reserved
            };
            // SAFETY: the caller supplies exclusive startup custody. Validate
            // all transferred and reserved roles before adopting any handle.
            unsafe { harn_vm::process_sandbox::DescriptorTransfer::inherited(numbers, &reserved)? }
        };
        #[cfg(not(target_os = "linux"))]
        if !request.pinned_verifier_descriptors.is_empty() {
            return Err(io::Error::other(
                "isolated source verifier descriptor transport is unmeasured on this platform",
            ));
        }
        let mut stage_relay = None;
        if let Some(command) = request.stage_output_command.take() {
            let slots = inherited_descriptors.reserve_owned_slots(
                &request.stage_output_descriptors,
                harn_vm::shells::FinalOutputStage::descriptor_slots(),
            )?;
            if let Some(slots) = slots {
                let descriptors: [i32; 3] = slots
                    .as_slice()
                    .try_into()
                    .map_err(|_| io::Error::other("invalid stage descriptor assignment"))?;
                let (relay, assigned) =
                    stage_relay::StageRelay::prepare(&mut inherited_descriptors, descriptors)?;
                let payload_slots = inherited_descriptors.reserve_owned_slots(
                    &assigned,
                    harn_vm::shells::FinalOutputStage::descriptor_slots(),
                )?;
                if let Some(payload_slots) = payload_slots {
                    let payload: [i32; 3] = payload_slots
                        .as_slice()
                        .try_into()
                        .map_err(|_| io::Error::other("invalid relay descriptor assignment"))?;
                    if command.render(&mut request.args, payload).is_some() {
                        request.stage_output_descriptors = payload_slots;
                        stage_relay = Some(relay);
                    } else {
                        inherited_descriptors.discard_owned(&payload_slots);
                        request.stage_output_descriptors.clear();
                    }
                } else {
                    inherited_descriptors.discard_owned(&assigned);
                    request.stage_output_descriptors.clear();
                }
            } else {
                inherited_descriptors.discard_owned(&request.stage_output_descriptors);
                request.stage_output_descriptors.clear();
            }
        }
        Ok(Self {
            request,
            inherited_descriptors,
            stage_relay,
        })
    }

    pub(super) fn into_command(self) -> io::Result<PreparedPayload> {
        let request = self.request;
        let mut command = Command::new(OsString::from_vec(request.program));
        command.args(request.args.into_iter().map(OsString::from_vec));
        if let Some(cwd) = request.cwd {
            command.current_dir(PathBuf::from(OsString::from_vec(cwd)));
        }
        if request.env_clear {
            command.env_clear();
        }
        for (key, value) in request.env {
            let key = OsString::from_vec(key);
            match value {
                Some(value) => {
                    command.env(key, OsString::from_vec(value));
                }
                None => {
                    command.env_remove(key);
                }
            }
        }
        command
            .env_remove(MODE_ENV)
            .stdin(if request.payload_stdin {
                Stdio::piped()
            } else {
                Stdio::null()
            });
        let output_capture = match request.output_capture {
            GuardianOutputCapture::Pipe => OutputCapture::Pipe,
            GuardianOutputCapture::File {
                stdout_path,
                stderr_path,
            } => OutputCapture::File {
                stdout_path: PathBuf::from(OsString::from_vec(stdout_path)),
                stderr_path: PathBuf::from(OsString::from_vec(stderr_path)),
            },
        };
        if let Some(relay) = &self.stage_relay {
            relay.configure(&mut command)?;
        } else {
            crate::process::real::configure_output_capture(&mut command, &output_capture)
                .map_err(io::Error::other)?;
        }
        apply_confinement(&mut command, request.confinement)?;
        #[cfg(target_os = "linux")]
        if !request.pinned_verifier_descriptors.is_empty() {
            harn_vm::verifier_provenance::validate_pinned_environment(&command, request.env_clear)
                .map_err(io::Error::other)?;
        }
        if self.inherited_descriptors.count() != 0 {
            self.inherited_descriptors.attach(&mut command);
        }
        Ok(PreparedPayload {
            command,
            cleanup_token: request.cleanup_token,
            stage_relay: self.stage_relay,
        })
    }
}
