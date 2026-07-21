//! `Content-Length` message framing for the DAP wire protocol.
//!
//! DAP (like LSP) frames each JSON message with an HTTP-style header block
//! terminated by a blank line:
//!
//! ```text
//! Content-Length: 42\r\n
//! \r\n
//! {"seq":1,...}
//! ```
//!
//! Every adapter read and write goes through this module so the header
//! casing, the frame bound, and the desync rules have exactly one
//! definition. Header names are matched case-insensitively per the HTTP
//! header conventions the format inherits; unrecognized headers (e.g.
//! `Content-Type`) are skipped.

use std::io::{self, BufRead, Write};
use std::sync::{Arc, Mutex};

/// Largest frame body the adapter will read. A client that announces more
/// than this is malfunctioning or hostile, so the read path refuses it
/// rather than allocating the buffer.
///
/// `harn-serve`'s async MCP JSON-RPC stdio transport
/// (`transport::jsonrpc_stdio`) frames the same base wire format
/// (Content-Length + CRLF) and enforces the identical 16 MiB bound as its
/// own `MAX_JSONRPC_FRAME_BYTES`. The two are deliberately *not* shared:
/// that reader is built on `tokio::io::AsyncBufRead` while this one is
/// synchronous `std::io::BufRead`, and the only crates both depend on are
/// the language crates (harn-vm/parser/modules) where a stdio-transport
/// helper would be a wrong-direction edge. Keep the bounds in lockstep by
/// hand.
pub const MAX_DAP_FRAME_BYTES: usize = 16 * 1024 * 1024;

/// Stdout shared by the response loop and the host bridge. Both write
/// whole frames under this lock so headers and bodies never interleave.
pub type SharedWriter = Arc<Mutex<Box<dyn Write + Send>>>;

/// Read one frame's header block and return its announced body length.
///
/// Returns `Ok(None)` at a clean EOF. Header lines that are not
/// `Content-Length` are ignored. A blank line before any `Content-Length`
/// has been seen is not a frame boundary — it is skipped, which keeps a
/// stray newline between frames from being read as an empty message.
fn read_content_length<R: BufRead>(reader: &mut R) -> io::Result<Option<usize>> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            match content_length {
                Some(length) => return Ok(Some(length)),
                None => continue,
            }
        }
        let Some((name, value)) = trimmed.split_once(':') else {
            continue;
        };
        if !name.trim().eq_ignore_ascii_case("Content-Length") {
            continue;
        }
        // An unparseable length (non-numeric, or wider than usize) leaves
        // us unable to find the frame boundary. Failing here is what keeps
        // the body from being misread as the next header block.
        let length: usize = value.trim().parse().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("malformed DAP Content-Length {:?}", value.trim()),
            )
        })?;
        content_length = Some(bounded_frame_length(length)?);
    }
}

fn bounded_frame_length(content_length: usize) -> io::Result<usize> {
    if content_length > MAX_DAP_FRAME_BYTES {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "DAP Content-Length {content_length} exceeds limit {MAX_DAP_FRAME_BYTES} bytes"
            ),
        ))
    } else {
        Ok(content_length)
    }
}

/// Read one complete frame body, bounded by [`MAX_DAP_FRAME_BYTES`].
///
/// Returns `Ok(None)` at a clean EOF (including a truncated body, which is
/// what a client that dies mid-frame produces). Empty frames are skipped
/// rather than surfaced as zero-byte bodies.
pub fn read_frame<R: BufRead>(reader: &mut R) -> io::Result<Option<Vec<u8>>> {
    loop {
        let Some(content_length) = read_content_length(reader)? else {
            return Ok(None);
        };
        if content_length == 0 {
            continue;
        }
        let mut body = vec![0u8; content_length];
        if reader.read_exact(&mut body).is_err() {
            return Ok(None);
        }
        return Ok(Some(body));
    }
}

/// Write one framed message to `writer`.
pub fn write_frame<W: Write + ?Sized>(writer: &mut W, body: &[u8]) -> io::Result<()> {
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(body)?;
    writer.flush()
}

/// Serialize `value` and write it as one frame under the shared stdout
/// lock, so concurrent writers cannot interleave header and body.
pub fn write_json_frame<T: serde::Serialize>(stdout: &SharedWriter, value: &T) -> io::Result<()> {
    let body = serde_json::to_vec(value).map_err(|e| io::Error::other(format!("encode: {e}")))?;
    let mut guard = stdout
        .lock()
        .map_err(|_| io::Error::other("stdout mutex poisoned"))?;
    write_frame(&mut **guard, &body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_all(input: &[u8]) -> io::Result<Option<Vec<u8>>> {
        read_frame(&mut io::BufReader::new(input))
    }

    fn frame(body: &str) -> Vec<u8> {
        let mut out = Vec::new();
        write_frame(&mut out, body.as_bytes()).expect("write");
        out
    }

    #[test]
    fn round_trips_a_frame() {
        let encoded = frame(r#"{"seq":1}"#);
        assert_eq!(encoded, b"Content-Length: 9\r\n\r\n{\"seq\":1}");
        assert_eq!(
            read_all(&encoded).expect("read").expect("frame"),
            br#"{"seq":1}"#
        );
    }

    #[test]
    fn header_names_match_case_insensitively() {
        let input = b"content-length: 2\r\n\r\nhi";
        assert_eq!(read_all(input).expect("read").expect("frame"), b"hi");
    }

    #[test]
    fn unrelated_headers_are_skipped() {
        let input = b"Content-Type: application/vscode-jsonrpc\r\nContent-Length: 2\r\n\r\nhi";
        assert_eq!(read_all(input).expect("read").expect("frame"), b"hi");
    }

    #[test]
    fn accepts_a_frame_at_exactly_the_limit() {
        // Only the header is exercised against the bound here; allocating a
        // full 16 MiB body would make this test needlessly heavy.
        let header = format!("Content-Length: {MAX_DAP_FRAME_BYTES}\r\n\r\n");
        let mut reader = io::BufReader::new(header.as_bytes());
        assert_eq!(
            read_content_length(&mut reader).expect("at limit"),
            Some(MAX_DAP_FRAME_BYTES)
        );
    }

    #[test]
    fn rejects_a_frame_over_the_limit_without_allocating() {
        let header = format!("Content-Length: {}\r\n\r\n", MAX_DAP_FRAME_BYTES + 1);
        let error = read_all(header.as_bytes()).expect_err("oversized frame");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("exceeds limit"), "{error}");
    }

    #[test]
    fn rejects_a_length_too_wide_for_usize() {
        // Would previously parse-fail silently, leaving the body to be read
        // as the next header block.
        let input = b"Content-Length: 999999999999999999999999\r\n\r\n{}";
        let error = read_all(input).expect_err("overflowing length");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn truncated_body_reports_eof_rather_than_a_partial_frame() {
        let input = b"Content-Length: 64\r\n\r\n{\"seq\":1}";
        assert_eq!(read_all(input).expect("read"), None);
    }

    #[test]
    fn truncated_header_block_reports_eof() {
        assert_eq!(read_all(b"Content-Length: 12\r\n").expect("read"), None);
    }

    #[test]
    fn empty_input_is_a_clean_eof() {
        assert_eq!(read_all(b"").expect("read"), None);
    }

    #[test]
    fn reads_consecutive_frames_from_one_stream() {
        let mut stream = frame("ab");
        stream.extend(frame("cde"));
        let mut reader = io::BufReader::new(stream.as_slice());
        assert_eq!(
            read_frame(&mut reader).expect("read").expect("first"),
            b"ab"
        );
        assert_eq!(
            read_frame(&mut reader).expect("read").expect("second"),
            b"cde"
        );
        assert_eq!(read_frame(&mut reader).expect("read"), None);
    }
}
