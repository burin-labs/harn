//! Explicit libtest dispatch and separation of harness output from payloads.

#[cfg(unix)]
use super::*;

#[cfg(unix)]
#[doc(hidden)]
pub use harn_vm::stdlib::macros as builtin_support;

#[doc(hidden)]
#[cfg(unix)]
#[builtin_support::distributed_slice]
#[linkme(crate = builtin_support::linkme)]
pub static DISPATCHERS: [&[&str]] = [..];

#[cfg(unix)]
pub(super) fn arguments() -> Result<Option<Vec<OsString>>, ProcessError> {
    match &*DISPATCHERS {
        [] => Ok(None),
        [args] => Ok(Some(args.iter().map(OsString::from).collect())),
        _ => Err(ProcessError::InvalidArgv(
            "more than one test process guardian dispatcher is linked".into(),
        )),
    }
}

#[cfg(unix)]
fn boundary(token: &str) -> Vec<u8> {
    [
        b"\0harn-guardian-stdout\0".as_slice(),
        token.as_bytes(),
        b"\0",
    ]
    .concat()
}

#[cfg(unix)]
pub(super) fn write_boundary(token: &str) -> io::Result<()> {
    let mut stdout = io::stdout();
    stdout.write_all(&boundary(token))?;
    stdout.flush()
}

#[cfg(unix)]
pub(super) fn consume_boundary(reader: &mut impl Read, token: &str) -> io::Result<()> {
    let marker = boundary(token);
    let mut prelude = Vec::new();
    for _ in 0..65_536 {
        let mut byte = [0];
        reader.read_exact(&mut byte)?;
        prelude.push(byte[0]);
        if prelude.ends_with(&marker) {
            return Ok(());
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "test guardian stdout boundary was not observed",
    ))
}

/// Declare the process guardian once at the root of a libtest executable.
///
/// The immutable declaration is available to every host worker thread. Normal
/// executables use `run_if_requested` at the beginning of `main` instead.
#[macro_export]
macro_rules! process_guardian_test_fixture {
    () => {
        #[cfg(unix)]
        #[test]
        fn __harn_process_guardian_fixture() {
            if $crate::process::owner_death::guardian_requested() {
                $crate::process::owner_death::run_guardian_from_pipe()
                    .expect("run test process guardian");
            }
        }

        #[cfg(unix)]
        const _: () = {
            #[$crate::process::owner_death::test_reexec::builtin_support::distributed_slice(
                $crate::process::owner_death::test_reexec::DISPATCHERS
            )]
            #[linkme(crate = $crate::process::owner_death::test_reexec::builtin_support::linkme)]
            static DISPATCHER: &[&str] =
                &["--exact", "__harn_process_guardian_fixture", "--nocapture"];
        };
    };
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn the_request_boundary_preserves_exact_payload_bytes() {
        let token = "exact-guardian-request";
        let mut bytes = b"running 1 test\n\nrunning 1 test\n".to_vec();
        bytes.extend(boundary(token));
        bytes.extend(b"hello\n\0payload");
        let mut reader = io::Cursor::new(bytes);
        consume_boundary(&mut reader, token).unwrap();
        let mut payload = Vec::new();
        reader.read_to_end(&mut payload).unwrap();
        assert_eq!(payload, b"hello\n\0payload");
    }

    #[test]
    fn absent_foreign_and_unbounded_boundaries_refuse() {
        for bytes in [Vec::new(), boundary("another-request"), vec![b'x'; 65_536]] {
            assert!(consume_boundary(&mut io::Cursor::new(bytes), "expected-request").is_err());
        }
    }
}
