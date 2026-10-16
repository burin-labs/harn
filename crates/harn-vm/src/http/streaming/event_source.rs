//! Reconnecting Server-Sent Events client behind `sse_connect`.
//!
//! The stream sends the request with `Accept: text/event-stream`, yields
//! [`SseEvent::Open`] once the server answers `200` with an event-stream
//! content type, and then yields parsed messages. A transport error or a
//! server-closed body is reported to the caller and schedules a reconnect
//! that carries `Last-Event-ID`, backing off exponentially from 300ms to 5s.
//! A server `retry:` field resets the starting delay. A non-200 status or a
//! wrong content type is terminal.

use std::fmt;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use eventsource_stream::{EventStreamError, Eventsource};
use futures::future::BoxFuture;
use futures::stream::BoxStream;
use futures::{FutureExt, Stream};
use reqwest::header::{HeaderValue, ACCEPT, CONTENT_TYPE};
use reqwest::{RequestBuilder, Response, StatusCode};

pub(super) use eventsource_stream::Event as SseMessage;

const INITIAL_RETRY_DELAY: Duration = Duration::from_millis(300);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(5);
const LAST_EVENT_ID: &str = "last-event-id";

type ResponseFuture = BoxFuture<'static, Result<Response, reqwest::Error>>;
type MessageStream = BoxStream<'static, Result<SseMessage, EventStreamError<reqwest::Error>>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SseEvent {
    Open,
    Message(SseMessage),
}

#[derive(Debug)]
pub(super) enum SseError {
    RequestNotCloneable,
    Transport(reqwest::Error),
    Utf8(std::string::FromUtf8Error),
    Parser(String),
    InvalidStatusCode(StatusCode),
    InvalidContentType(HeaderValue),
    InvalidLastEventId(String),
    StreamEnded,
}

impl fmt::Display for SseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RequestNotCloneable => f.write_str("expected a cloneable request"),
            Self::Transport(error) => error.fmt(f),
            Self::Utf8(error) => error.fmt(f),
            Self::Parser(error) => f.write_str(error),
            Self::InvalidStatusCode(status) => write!(f, "Invalid status code: {status}"),
            Self::InvalidContentType(value) => write!(f, "Invalid header value: {value:?}"),
            Self::InvalidLastEventId(id) => write!(f, "Invalid `Last-Event-ID`: {id}"),
            Self::StreamEnded => f.write_str("Stream ended"),
        }
    }
}

impl From<EventStreamError<reqwest::Error>> for SseError {
    fn from(error: EventStreamError<reqwest::Error>) -> Self {
        match error {
            EventStreamError::Utf8(error) => Self::Utf8(error),
            EventStreamError::Parser(error) => Self::Parser(error.to_string()),
            EventStreamError::Transport(error) => Self::Transport(error),
        }
    }
}

pub(super) struct EventSource {
    builder: RequestBuilder,
    next_response: Option<ResponseFuture>,
    messages: Option<MessageStream>,
    delay: Option<Pin<Box<tokio::time::Sleep>>>,
    closed: bool,
    last_event_id: String,
    reconnect_start: Duration,
    reconnect_max: Duration,
    last_delay: Option<Duration>,
}

impl EventSource {
    pub(super) fn new(builder: RequestBuilder) -> Result<Self, SseError> {
        let builder = builder.header(ACCEPT, HeaderValue::from_static("text/event-stream"));
        let first = builder
            .try_clone()
            .ok_or(SseError::RequestNotCloneable)?
            .send()
            .boxed();
        Ok(Self {
            builder,
            next_response: Some(first),
            messages: None,
            delay: None,
            closed: false,
            last_event_id: String::new(),
            reconnect_start: INITIAL_RETRY_DELAY,
            reconnect_max: MAX_RETRY_DELAY,
            last_delay: None,
        })
    }

    /// Stops the stream; later polls end it without reconnecting.
    pub(super) fn close(&mut self) {
        self.closed = true;
    }

    fn reconnect(&mut self) -> Result<(), SseError> {
        self.messages = None;
        let last_event_id = HeaderValue::from_str(&self.last_event_id)
            .map_err(|_| SseError::InvalidLastEventId(self.last_event_id.clone()))?;
        let request = self
            .builder
            .try_clone()
            .ok_or(SseError::RequestNotCloneable)?
            .header(LAST_EVENT_ID, last_event_id);
        self.next_response = Some(request.send().boxed());
        Ok(())
    }

    fn schedule_reconnect(&mut self) {
        self.next_response = None;
        self.messages = None;
        let delay = self.last_delay.map_or(self.reconnect_start, |last| {
            last.mul_f64(2.0).min(self.reconnect_max)
        });
        self.last_delay = Some(delay);
        self.delay = Some(Box::pin(tokio::time::sleep(delay)));
    }

    fn open(&mut self, response: Response) {
        self.last_delay = None;
        let mut messages = response.bytes_stream().eventsource();
        messages.set_last_event_id(self.last_event_id.clone());
        self.messages = Some(Box::pin(messages));
    }

    fn observe(&mut self, message: &SseMessage) {
        self.last_event_id.clone_from(&message.id);
        if let Some(retry) = message.retry {
            self.reconnect_start = retry;
            self.reconnect_max = self.reconnect_max.max(retry);
        }
    }
}

fn check_response(response: Response) -> Result<Response, SseError> {
    if response.status() != StatusCode::OK {
        return Err(SseError::InvalidStatusCode(response.status()));
    }
    let Some(content_type) = response.headers().get(CONTENT_TYPE) else {
        return Err(SseError::InvalidContentType(HeaderValue::from_static("")));
    };
    let is_event_stream = content_type
        .to_str()
        .ok()
        .and_then(|value| value.split(';').next())
        .is_some_and(|essence| essence.trim().eq_ignore_ascii_case("text/event-stream"));
    if is_event_stream {
        Ok(response)
    } else {
        Err(SseError::InvalidContentType(content_type.clone()))
    }
}

impl Stream for EventSource {
    type Item = Result<SseEvent, SseError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.closed {
            return Poll::Ready(None);
        }

        if let Some(delay) = this.delay.as_mut() {
            if delay.poll_unpin(cx).is_pending() {
                return Poll::Pending;
            }
            this.delay = None;
            if let Err(error) = this.reconnect() {
                this.closed = true;
                return Poll::Ready(Some(Err(error)));
            }
        }

        if let Some(response) = this.next_response.as_mut() {
            let response = match response.poll_unpin(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(response) => response,
            };
            this.next_response = None;
            return match response {
                Ok(response) => match check_response(response) {
                    Ok(response) => {
                        this.open(response);
                        Poll::Ready(Some(Ok(SseEvent::Open)))
                    }
                    Err(error) => {
                        this.closed = true;
                        Poll::Ready(Some(Err(error)))
                    }
                },
                Err(error) => {
                    this.schedule_reconnect();
                    Poll::Ready(Some(Err(SseError::Transport(error))))
                }
            };
        }

        let Some(messages) = this.messages.as_mut() else {
            return Poll::Ready(None);
        };
        match messages.as_mut().poll_next(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Some(Ok(message))) => {
                this.observe(&message);
                Poll::Ready(Some(Ok(SseEvent::Message(message))))
            }
            Poll::Ready(Some(Err(error))) => {
                this.schedule_reconnect();
                Poll::Ready(Some(Err(error.into())))
            }
            Poll::Ready(None) => {
                this.schedule_reconnect();
                Poll::Ready(Some(Err(SseError::StreamEnded)))
            }
        }
    }
}
