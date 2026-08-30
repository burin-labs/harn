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
}

impl<R> RawResponseReader<R> {
    pub(crate) fn new(inner: R, responses: RawResponseLog) -> Self {
        Self {
            inner,
            partial_line: Vec::new(),
            responses,
        }
    }

    fn record_bytes(&mut self, bytes: &[u8]) {
        self.partial_line.extend_from_slice(bytes);
        while let Some(end) = self.partial_line.iter().position(|byte| *byte == b'\n') {
            let mut line = self.partial_line.drain(..=end).collect::<Vec<_>>();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if let Ok(response) = serde_json::from_slice(&line) {
                self.responses.record(response);
            }
        }
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for RawResponseReader<R> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let before = buf.filled().len();
        let result = Pin::new(&mut this.inner).poll_read(cx, buf);
        if let Poll::Ready(Ok(())) = &result {
            let bytes = buf.filled()[before..].to_vec();
            this.record_bytes(&bytes);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    #[tokio::test]
    async fn records_complete_responses_across_partial_reads() {
        let responses = RawResponseLog::default();
        let marker = responses.mark();
        let (mut writer, reader) = tokio::io::duplex(8);
        let mut reader = RawResponseReader::new(reader, responses.clone());
        let write = tokio::spawn(async move {
            tokio::io::AsyncWriteExt::write_all(
                &mut writer,
                b"{\"jsonrpc\":\"2.0\",\"id\":7,\"result\":{\"future\":true}}\n",
            )
            .await
            .unwrap();
        });
        let mut sink = Vec::new();
        reader.read_to_end(&mut sink).await.unwrap();
        write.await.unwrap();
        assert_eq!(
            responses.result_after(marker),
            Ok(serde_json::json!({"future": true}))
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
}
