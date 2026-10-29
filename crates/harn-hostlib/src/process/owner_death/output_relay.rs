use std::io::{self, Read, Write};

/// Forward partial lines immediately and flush them before signal exit.
pub(super) fn copy(mut source: impl Read, mut destination: impl Write) -> io::Result<()> {
    let mut buffer = [0_u8; 8192];
    loop {
        let count = match source.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(count) => count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        destination.write_all(&buffer[..count])?;
        destination.flush()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Captured {
        bytes: Vec<u8>,
        flushes: usize,
    }

    impl Write for Captured {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            Ok(())
        }
    }

    #[test]
    fn flushes_partial_lines_before_the_pipe_closes() {
        let mut output = Captured::default();
        copy(&b"before"[..], &mut output).unwrap();
        assert_eq!(output.bytes, b"before");
        assert_eq!(output.flushes, 1);
    }
}
