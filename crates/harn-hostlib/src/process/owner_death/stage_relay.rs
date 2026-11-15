use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::process::{Command, Stdio};

use harn_vm::process_sandbox::DescriptorTransfer;

/// Keep prefix and relayed stage bytes on one kernel-ordered aggregate stream.
/// The separate stage destination carries only the bytes read from that stage.
pub(super) struct StageRelay {
    stdout: RelayStream,
    stderr: RelayStream,
    aggregate_stdout: UnixStream,
    aggregate_stderr: UnixStream,
}

pub(super) struct RelayStream {
    source: UnixStream,
    stage: File,
    aggregate: File,
}

impl StageRelay {
    pub(super) fn prepare(
        transfer: &mut DescriptorTransfer,
        descriptors: [i32; 3],
    ) -> io::Result<(Self, [i32; 3])> {
        let mut inherited = transfer.take_owned(&descriptors)?.into_iter();
        let stdout = inherited.next().expect("validated stdout role");
        let stderr = inherited.next().expect("validated stderr role");
        let entry = inherited.next().expect("validated entry role");
        let stdout = File::from(high_descriptor(stdout)?);
        let stderr = File::from(high_descriptor(stderr)?);
        let (stdout_source, stdout_writer) = stage_pair()?;
        let (stderr_source, stderr_writer) = stage_pair()?;
        let assigned = [
            stdout_writer.as_raw_fd(),
            stderr_writer.as_raw_fd(),
            entry.as_raw_fd(),
        ];
        transfer.extend_owned(vec![stdout_writer, stderr_writer, entry]);
        let (aggregate_stdout, stdout_aggregate) = aggregate_pair()?;
        let (aggregate_stderr, stderr_aggregate) = aggregate_pair()?;
        Ok((
            Self {
                stdout: RelayStream {
                    source: stdout_source,
                    stage: stdout,
                    aggregate: stdout_aggregate,
                },
                stderr: RelayStream {
                    source: stderr_source,
                    stage: stderr,
                    aggregate: stderr_aggregate,
                },
                aggregate_stdout,
                aggregate_stderr,
            },
            assigned,
        ))
    }

    pub(super) fn configure(&self, command: &mut Command) -> io::Result<()> {
        command.stdout(Stdio::from(self.stdout.aggregate.try_clone()?));
        command.stderr(Stdio::from(self.stderr.aggregate.try_clone()?));
        Ok(())
    }

    pub(super) fn into_parts(self) -> (UnixStream, UnixStream, [RelayStream; 2]) {
        (
            self.aggregate_stdout,
            self.aggregate_stderr,
            [self.stdout, self.stderr],
        )
    }
}

impl RelayStream {
    pub(super) fn run(mut self) -> io::Result<()> {
        let mut buffer = [0; 8192];
        loop {
            let count = match self.source.read(&mut buffer) {
                Ok(0) => return Ok(()),
                Ok(count) => count,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            };
            // Prefix writes are already queued on this same aggregate socket.
            self.aggregate.write_all(&buffer[..count])?;
            self.stage.write_all(&buffer[..count])?;
        }
    }
}

fn high_descriptor(descriptor: OwnedFd) -> io::Result<OwnedFd> {
    let mut transfer = DescriptorTransfer::new(vec![descriptor]);
    let numbers = transfer.numbers();
    let relocated = transfer
        .reserve_owned_slots(&numbers, 10..=i32::MAX)?
        .ok_or_else(|| io::Error::other("no private relay descriptor available"))?;
    transfer
        .take_owned(&relocated)?
        .pop()
        .ok_or_else(|| io::Error::other("private relay descriptor missing"))
}

fn stage_pair() -> io::Result<(UnixStream, OwnedFd)> {
    let (reader, writer) = UnixStream::pair()?;
    // The payload alone needs portable shell slots. Keep relay readers out of
    // that range before allocating the next pair.
    Ok((
        UnixStream::from(high_descriptor(reader.into())?),
        writer.into(),
    ))
}

fn aggregate_pair() -> io::Result<(UnixStream, File)> {
    let (reader, writer) = UnixStream::pair()?;
    Ok((
        UnixStream::from(high_descriptor(reader.into())?),
        File::from(high_descriptor(writer.into())?),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn stage_read_ahead_keeps_prefix_bytes_first_on_the_aggregate_channel() {
        let (source, mut stage_writer) = UnixStream::pair().unwrap();
        let (mut aggregate_reader, aggregate_writer) = UnixStream::pair().unwrap();
        let (mut stage_reader, private_writer) = UnixStream::pair().unwrap();
        let mut aggregate = File::from(OwnedFd::from(aggregate_writer));
        aggregate.write_all(b"prefix-").unwrap();
        let pump = RelayStream {
            source,
            stage: File::from(OwnedFd::from(private_writer)),
            aggregate,
        };
        let (tx, rx) = mpsc::channel();
        let thread = harn_parser::runtime_stack::spawn(move || {
            tx.send(pump.run()).unwrap();
        });
        stage_writer.write_all(b"stage").unwrap();
        drop(stage_writer);
        harn_clock::test_support::recv_within("stage forwarded before aggregate consumption", &rx)
            .unwrap();
        thread.join().unwrap();
        let mut aggregate = Vec::new();
        aggregate_reader.read_to_end(&mut aggregate).unwrap();
        let mut stage = Vec::new();
        stage_reader.read_to_end(&mut stage).unwrap();
        assert_eq!(aggregate, b"prefix-stage");
        assert_eq!(stage, b"stage");
    }

    #[test]
    fn failed_private_stage_relay_leaves_capture_unconfirmed() {
        for failed in [false, true] {
            let (source, mut stage_writer) = UnixStream::pair().unwrap();
            let (mut aggregate_reader, aggregate_writer) = UnixStream::pair().unwrap();
            let (stage_reader, private_writer) = UnixStream::pair().unwrap();
            let mut stage_reader = Some(stage_reader);
            if failed {
                drop(stage_reader.take());
            }
            let pump = RelayStream {
                source,
                stage: File::from(OwnedFd::from(private_writer)),
                aggregate: File::from(OwnedFd::from(aggregate_writer)),
            };
            stage_writer.write_all(b"stage").unwrap();
            drop(stage_writer);
            let (tx, rx) = mpsc::channel();
            let thread = harn_parser::runtime_stack::spawn(move || {
                tx.send(pump.run().is_ok()).unwrap();
            });
            let complete = harn_clock::test_support::recv_within("stage relay settled", &rx);
            thread.join().unwrap();
            assert_eq!(complete, !failed);
            let mut aggregate = Vec::new();
            aggregate_reader.read_to_end(&mut aggregate).unwrap();
            assert_eq!(aggregate, b"stage");
            if let Some(mut stage_reader) = stage_reader {
                let mut stage = Vec::new();
                stage_reader.read_to_end(&mut stage).unwrap();
                assert_eq!(stage, b"stage");
            }
        }
    }
}
