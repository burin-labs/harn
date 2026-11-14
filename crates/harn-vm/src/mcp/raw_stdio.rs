//! Lossless observation of newline-delimited stdio JSON-RPC responses.

use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use serde_json::Value;
use tokio::io::{AsyncRead, ReadBuf};

const RETAINED_RESPONSES: usize = 32;

#[derive(Clone, Default)]
pub(crate) struct RawResponseLog {
    state: Arc<Mutex<RawResponseState>>,
}

#[derive(Default)]
struct RawResponseState {
    sequence: u64,
    responses: VecDeque<(u64, Value)>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RawResponseError {
    ResponseCount(usize),
    MissingResult,
}

impl RawResponseLog {
    pub(crate) fn mark(&self) -> u64 {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .sequence
    }

    pub(crate) fn result_after(&self, marker: u64) -> Result<Value, RawResponseError> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let responses = state
            .responses
            .iter()
            .filter(|(sequence, _)| *sequence > marker)
            .map(|(_, response)| response)
            .collect::<Vec<_>>();
        if responses.len() != 1 {
            return Err(RawResponseError::ResponseCount(responses.len()));
        }
        responses[0]
            .get("result")
            .cloned()
            .ok_or(RawResponseError::MissingResult)
    }

    fn record(&self, response: Value) {
        if response.get("id").is_none()
            || (response.get("result").is_none() && response.get("error").is_none())
        {
            return;
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.sequence = state.sequence.saturating_add(1);
        let sequence = state.sequence;
        state.responses.push_back((sequence, response));
        while state.responses.len() > RETAINED_RESPONSES {
            state.responses.pop_front();
        }
    }
}

pub(crate) struct RawResponseReader<R> {
    inner: R,
    partial_line: Vec<u8>,
    responses: RawResponseLog,
    failed: bool,
}

impl<R> RawResponseReader<R> {
    pub(crate) fn new(inner: R, responses: RawResponseLog) -> Self {
        Self {
            inner,
            partial_line: Vec::new(),
            responses,
            failed: false,
        }
    }

    fn record_bytes(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        for segment in bytes.split_inclusive(|byte| *byte == b'\n') {
            // The lossless observer must obey the same bound as the ordinary
            // SDK reader, including while the SDK is discarding a long line.
            if self.partial_line.len().saturating_add(segment.len())
                > super::MCP_MAX_STDIO_LINE_BYTES
            {
                self.partial_line.clear();
                self.failed = true;
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "MCP message exceeds the SDK frame limit",
                ));
            }
            self.partial_line.extend_from_slice(segment);
            if segment.last() == Some(&b'\n') {
                self.partial_line.pop();
                if self.partial_line.last() == Some(&b'\r') {
                    self.partial_line.pop();
                }
                if let Ok(response) = super::protocol::parse_jsonrpc_message(&self.partial_line) {
                    self.responses.record(response);
                }
                self.partial_line.clear();
            }
        }
        Ok(())
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for RawResponseReader<R> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        if this.failed {
            return Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "MCP message exceeds the SDK frame limit",
            )));
        }
        let before = buf.filled().len();
        let result = Pin::new(&mut this.inner).poll_read(cx, buf);
        if matches!(&result, Poll::Ready(Ok(()))) {
            let bytes = buf.filled()[before..].to_vec();
            if let Err(error) = this.record_bytes(&bytes) {
                // An AsyncRead error must not also report newly filled bytes.
                buf.set_filled(before);
                return Poll::Ready(Err(error));
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    #[tokio::test]
    async fn records_complete_responses_and_input_order_across_partial_reads() {
        let responses = RawResponseLog::default();
        let marker = responses.mark();
        let (mut writer, reader) = tokio::io::duplex(8);
        let mut reader = RawResponseReader::new(reader, responses.clone());
        let write = tokio::spawn(async move {
            tokio::io::AsyncWriteExt::write_all(
                &mut writer,
                b"{\"jsonrpc\":\"2.0\",\"id\":7,\"result\":{\"resultType\":\"input_required\",\"inputRequests\":{\"form\":{\"method\":\"elicitation/create\",\"params\":{\"mode\":\"form\",\"message\":\"Choose\",\"requestedSchema\":{\"type\":\"object\",\"properties\":{\"zeta\":{\"type\":\"string\"},\"alpha\":{\"type\":\"integer\"}}}}}}}}\n",
            )
            .await
            .unwrap();
        });
        let mut sink = Vec::new();
        reader.read_to_end(&mut sink).await.unwrap();
        write.await.unwrap();
        let result = responses
            .result_after(marker)
            .expect("one complete response");
        assert_eq!(
            result["inputRequests"]["form"]["params"]
                [super::super::protocol::REQUESTED_SCHEMA_PROPERTY_ORDER],
            serde_json::json!(["zeta", "alpha"])
        );
    }

    #[test]
    fn ignores_notifications_and_bounds_retention() {
        let responses = RawResponseLog::default();
        responses.record(serde_json::json!({"jsonrpc": "2.0", "method": "notice"}));
        let marker = responses.mark();
        for id in 0..(RETAINED_RESPONSES + 4) {
            responses.record(serde_json::json!({"jsonrpc": "2.0", "id": id, "result": {"id": id}}));
        }
        let state = responses
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(state.sequence, (RETAINED_RESPONSES + 4) as u64);
        assert_eq!(state.responses.len(), RETAINED_RESPONSES);
        drop(state);
        assert_eq!(
            responses.result_after(marker),
            Err(RawResponseError::ResponseCount(RETAINED_RESPONSES))
        );
    }

    #[tokio::test]
    async fn an_unterminated_oversized_frame_cannot_grow_the_lossless_observer() {
        let limit = super::super::MCP_MAX_STDIO_LINE_BYTES;
        let mut reader = RawResponseReader::new(
            std::io::Cursor::new(vec![b'x'; limit + 1]),
            RawResponseLog::default(),
        );
        let mut sink = Vec::new();
        assert_eq!(
            reader.read_to_end(&mut sink).await.unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
        assert!(reader.failed && reader.partial_line.is_empty());
        assert!(sink.len() <= limit);
        assert_eq!(
            reader.read_to_end(&mut sink).await.unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
    }

    #[tokio::test]
    async fn newline_and_crlf_frame_boundaries_match_the_pinned_sdk_transport() {
        use rmcp::service::RoleClient;
        use rmcp::transport::{async_rw::AsyncRwTransport, Transport};

        let limit = super::super::MCP_MAX_STDIO_LINE_BYTES;
        assert!(super::super::MCP_STDIO_LINE_LIMIT_INCLUDES_TERMINATOR);
        for (ending, line_length, accepted) in [
            (b"\n".as_slice(), limit, true),
            (b"\n".as_slice(), limit + 1, false),
            (b"\r\n".as_slice(), limit, true),
            (b"\r\n".as_slice(), limit + 1, false),
        ] {
            let content_length = line_length - ending.len();
            let prefix = br#"{"jsonrpc":"2.0","id":7,"result":{"resultType":"complete","tools":[],"ttlMs":null,"_meta":{"padding":""#;
            let suffix = br#""}}}"#;
            let mut line = Vec::with_capacity(line_length);
            line.extend_from_slice(prefix);
            line.resize(content_length - suffix.len(), b'x');
            line.extend_from_slice(suffix);
            line.extend_from_slice(ending);
            let mut sdk = AsyncRwTransport::<RoleClient, _, _>::new(
                std::io::Cursor::new(line.clone()),
                tokio::io::sink(),
            );
            assert_eq!(
                sdk.receive().await.is_some(),
                accepted,
                "known SDK transport boundary for full line {line_length}"
            );
            let responses = RawResponseLog::default();
            let mut reader = RawResponseReader::new(std::io::empty(), responses.clone());
            assert_eq!(
                reader.record_bytes(&line).is_ok(),
                accepted,
                "observer disagrees with pinned SDK for full line {line_length}, ending {ending:?}"
            );
            if accepted {
                assert!(responses.result_after(0).unwrap()["_meta"]["padding"]
                    .as_str()
                    .is_some());
                let mut fragmented =
                    RawResponseReader::new(std::io::empty(), RawResponseLog::default());
                fragmented.record_bytes(&line[..line.len() - 1]).unwrap();
                fragmented.record_bytes(&line[line.len() - 1..]).unwrap();
            }
        }
    }
}
