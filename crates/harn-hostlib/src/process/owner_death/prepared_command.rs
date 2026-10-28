use super::*;

impl PreparedCommand {
    /// Admit the private startup request without claiming any descriptor twice.
    ///
    /// # Safety
    /// Every accepted descriptor must have been transferred exclusively to this
    /// guardian by the host launch. Shape/open checks cannot establish custody.
    pub(super) unsafe fn from_inherited_request(
        request: GuardianCommandRequest,
    ) -> io::Result<Self> {
        #[cfg(target_os = "linux")]
        let inherited_descriptors = {
            let mut numbers = request.pinned_verifier_descriptors.clone();
            if let Some(GuardianConfinement::Bubblewrap { descriptors }) = &request.confinement {
                numbers.extend(descriptors.iter().copied());
            }
            let mut reserved = request.cleanup_receipt_fd.into_iter().collect::<Vec<_>>();
            if matches!(
                &request.confinement,
                Some(
                    GuardianConfinement::BeforeExec { ruleset: true, .. }
                        | GuardianConfinement::AfterNamespace { ruleset: true }
                )
            ) {
                reserved.push(RULESET_FD);
            }
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
        Ok(Self {
            request,
            #[cfg(target_os = "linux")]
            inherited_descriptors,
        })
    }

    pub(super) fn into_command(self) -> io::Result<(Command, String)> {
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
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        apply_confinement(&mut command, request.confinement)?;
        #[cfg(target_os = "linux")]
        if !request.pinned_verifier_descriptors.is_empty() {
            harn_vm::verifier_provenance::validate_pinned_environment(&command, request.env_clear)
                .map_err(io::Error::other)?;
        }
        #[cfg(target_os = "linux")]
        if self.inherited_descriptors.count() != 0 {
            self.inherited_descriptors.attach(&mut command);
        }
        Ok((command, request.cleanup_token))
    }
}
