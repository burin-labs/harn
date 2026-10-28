//! The one Server-Sent Events framing owner for provider and MCP responses.
//!
//! Framing follows the WHATWG `text/event-stream` rules through
//! `eventsource-stream`: `data:` with or without a following space, CRLF/CR/LF
//! line ends, `:` comments, and several `data:` lines joined with `\n` into one
//! frame that is dispatched at the blank line. Consumers read whole frames
//! instead of re-deriving that from raw lines.
//!
//! Two deliberate leniencies, both for providers and gateways that do not
//! follow the spec to the letter:
//!
//! - A stream that ends, or fails, mid-event still delivers that event. The
//!   source is followed by a blank line, so the last `data:` line is not lost
//!   (the WHATWG rule would discard it) and bytes received before a read
//!   error still count as partial output.
//! - Consecutive `data:` lines with no blank line between them join into text
//!   that is not one JSON document while each line is. Such a frame is split
//!   back into one frame per line, which is how a line-per-event gateway
//!   reads. A payload that is valid JSON, however many lines it spans, stays
//!   whole.

use std::collections::VecDeque;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use eventsource_stream::{EventStream, EventStreamError, Eventsource};
use futures::{Stream, StreamExt};
use tokio::io::AsyncRead;
use tokio_util::io::ReaderStream;

/// One dispatched SSE frame.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SseFrame {
    /// The `event:` name, `None` for the default `message` event.
    pub event: Option<String>,
    /// The `data:` lines joined with `\n`.
    pub data: String,
}

/// Follows the source with a blank line when it ends or fails, so the event in
/// flight is dispatched before the end or the error is reported.
struct Terminated<S> {
    source: S,
    deferred_error: Option<io::Error>,
    finished: bool,
}

impl<S: Stream<Item = io::Result<Bytes>> + Unpin> Stream for Terminated<S> {
    type Item = io::Result<Bytes>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if let Some(error) = self.deferred_error.take() {
            return Poll::Ready(Some(Err(error)));
        }
        if self.finished {
            return Poll::Ready(None);
        }
        match self.source.poll_next_unpin(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Some(Ok(chunk))) => Poll::Ready(Some(Ok(chunk))),
            Poll::Ready(Some(Err(error))) => {
                self.finished = true;
                self.deferred_error = Some(error);
                Poll::Ready(Some(Ok(Bytes::from_static(b"\n\n"))))
            }
            Poll::Ready(None) => {
                self.finished = true;
                Poll::Ready(Some(Ok(Bytes::from_static(b"\n\n"))))
            }
        }
    }
}

/// Frames read from an async byte source.
pub(crate) struct SseFrames<R: AsyncRead + Unpin> {
    events: Pin<Box<EventStream<Terminated<ReaderStream<R>>>>>,
    queued: VecDeque<SseFrame>,
}

impl<R: AsyncRead + Unpin> SseFrames<R> {
    pub(crate) fn new(reader: R) -> Self {
        let source = Terminated {
            source: ReaderStream::new(reader),
            deferred_error: None,
            finished: false,
        };
        Self {
            events: Box::pin(source.eventsource()),
            queued: VecDeque::new(),
        }
    }

    /// The next frame, or `None` once the source is exhausted.
    pub(crate) async fn next(&mut self) -> io::Result<Option<SseFrame>> {
        loop {
            if let Some(frame) = self.queued.pop_front() {
                return Ok(Some(frame));
            }
            let event = match self.events.next().await {
                None => return Ok(None),
                Some(Ok(event)) => event,
                Some(Err(EventStreamError::Transport(error))) => return Err(error),
                Some(Err(error)) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        error.to_string(),
                    ))
                }
            };
            let name = (event.event != "message").then_some(event.event);
            self.queued.extend(split_line_per_event(name, event.data));
        }
    }
}

/// See the module note: split only when the joined text is not JSON but every
/// line is.
fn split_line_per_event(event: Option<String>, data: String) -> Vec<SseFrame> {
    let lines: Vec<&str> = data
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let line_per_event = lines.len() > 1
        && serde_json::from_str::<serde::de::IgnoredAny>(&data).is_err()
        && lines.iter().all(|line| {
            *line == "[DONE]" || serde_json::from_str::<serde::de::IgnoredAny>(line).is_ok()
        });
    if !line_per_event {
        return vec![SseFrame { event, data }];
    }
    lines
        .into_iter()
        .enumerate()
        .map(|(index, line)| SseFrame {
            event: if index == 0 { event.clone() } else { None },
            data: line.to_string(),
        })
        .collect()
}

/// Every frame of a fully buffered body. Malformed framing ends the list.
pub(crate) fn frames_in_body(body: &str) -> Vec<SseFrame> {
    futures::executor::block_on(async {
        let mut frames = SseFrames::new(body.as_bytes());
        let mut out = Vec::new();
        while let Ok(Some(frame)) = frames.next().await {
            out.push(frame);
        }
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;

    fn data(body: &str) -> Vec<String> {
        frames_in_body(body)
            .into_iter()
            .map(|frame| frame.data)
            .collect()
    }

    #[test]
    fn data_with_and_without_a_space_is_one_frame() {
        assert_eq!(
            data("data: {\"a\":1}\n\ndata:{\"b\":2}\n\n"),
            ["{\"a\":1}", "{\"b\":2}"]
        );
    }

    #[test]
    fn a_json_payload_split_across_data_lines_stays_one_frame() {
        assert_eq!(data("data: {\"a\":\ndata: 1}\n\n"), ["{\"a\":\n1}"]);
    }

    #[test]
    fn consecutive_data_lines_without_blank_lines_split_per_line() {
        assert_eq!(
            data("data: {\"a\":1}\ndata: {\"b\":2}\ndata: [DONE]\n"),
            ["{\"a\":1}", "{\"b\":2}", "[DONE]"]
        );
    }

    #[test]
    fn crlf_comments_and_event_names_are_framed() {
        let frames =
            frames_in_body(": keepalive\r\nevent: error\r\ndata: x\r\n\r\ndata: y\r\n\r\n");
        assert_eq!(
            frames,
            [
                SseFrame {
                    event: Some("error".into()),
                    data: "x".into()
                },
                SseFrame {
                    event: None,
                    data: "y".into()
                }
            ]
        );
    }

    #[test]
    fn a_final_frame_without_a_blank_line_is_delivered() {
        assert_eq!(data("data: a\n\ndata: b"), ["a", "b"]);
        assert_eq!(data("data: a\n\ndata: b\n"), ["a", "b"]);
    }

    #[test]
    fn frames_split_across_reads_are_reassembled() {
        let chunks = ["da", "ta:{\"a\"", ":1}\n", "\ndata: [DONE]\n\n"]
            .map(|chunk| Ok::<_, io::Error>(Bytes::from_static(chunk.as_bytes())));
        let reader = tokio_util::io::StreamReader::new(stream::iter(chunks));
        let frames = futures::executor::block_on(async {
            let mut frames = SseFrames::new(reader);
            let mut out = Vec::new();
            while let Some(frame) = frames.next().await.unwrap() {
                out.push(frame.data);
            }
            out
        });
        assert_eq!(frames, ["{\"a\":1}", "[DONE]"]);
    }

    #[test]
    fn an_event_in_flight_is_delivered_before_a_read_error() {
        let chunks = vec![
            Ok(Bytes::from_static(b"data: {\"a\":1}\n")),
            Err(io::Error::new(io::ErrorKind::TimedOut, "timed out")),
        ];
        let reader = tokio_util::io::StreamReader::new(stream::iter(chunks));
        futures::executor::block_on(async {
            let mut frames = SseFrames::new(reader);
            let first = frames.next().await.unwrap().unwrap();
            assert_eq!(first.data, "{\"a\":1}");
            let error = frames.next().await.unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        });
    }
}
