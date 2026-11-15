use std::io::{self, Read};

/// Only consecutive grammar-owned entries are meaningful. A complete prefix
/// can end before the final clause when the original && chain short-circuits.
pub(super) fn read(reader: &mut impl Read, total: usize) -> io::Result<Option<usize>> {
    if total == 0 {
        return Err(invalid());
    }
    let mut entered = 0usize;
    let mut number = 0usize;
    let mut digits = 0usize;
    let maximum_digits = total.to_string().len();
    let mut buffer = [0; 8192];
    loop {
        let count = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        for byte in &buffer[..count] {
            if *byte == b'\n' {
                if digits == 0 || entered.checked_add(1) != Some(number) || number > total {
                    return Err(invalid());
                }
                entered = number;
                number = 0;
                digits = 0;
            } else {
                if !byte.is_ascii_digit() || (digits == 0 && *byte == b'0') {
                    return Err(invalid());
                }
                number = number
                    .checked_mul(10)
                    .and_then(|number| number.checked_add(usize::from(*byte - b'0')))
                    .filter(|number| *number <= total)
                    .ok_or_else(invalid)?;
                digits += 1;
                if digits > maximum_digits {
                    return Err(invalid());
                }
            }
        }
    }
    if digits != 0 {
        return Err(invalid());
    }
    Ok((entered != 0).then_some(entered))
}

fn invalid() -> io::Error {
    io::Error::other("invalid command clause observation")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn complete_prefix_and_final_entries_are_distinct_from_no_observation() {
        assert_eq!(read(&mut Cursor::new(b""), 3).unwrap(), None);
        assert_eq!(read(&mut Cursor::new(b"1\n"), 3).unwrap(), Some(1));
        assert_eq!(read(&mut Cursor::new(b"1\n2\n"), 3).unwrap(), Some(2));
        assert_eq!(read(&mut Cursor::new(b"1\n2\n3\n"), 3).unwrap(), Some(3));
    }

    #[test]
    fn malformed_or_incomplete_observations_never_become_a_clause() {
        for bytes in [
            b"0\n".as_slice(),
            b"01\n",
            b"1",
            b"2\n",
            b"1\n1\n",
            b"1\n3\n",
            b"1\n2\n3\n4\n",
            b"1\n2",
            b"1\n\n",
            b"1\r\n",
            b"-1\n",
            b"99999999999999999999999999999\n",
        ] {
            assert!(read(&mut Cursor::new(bytes), 3).is_err(), "{bytes:?}");
        }
        assert!(read(&mut Cursor::new(b""), 0).is_err());
    }

    #[test]
    fn partial_bytes_followed_by_read_failure_are_not_an_observation() {
        struct Failure(bool);
        impl Read for Failure {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                if std::mem::replace(&mut self.0, true) {
                    return Err(io::Error::other("controlled observation failure"));
                }
                buffer[..2].copy_from_slice(b"1\n");
                Ok(2)
            }
        }
        assert!(read(&mut Failure(false), 3).is_err());
    }
}
