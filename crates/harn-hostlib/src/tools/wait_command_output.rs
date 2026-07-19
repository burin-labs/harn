//! Event-driven output matching for one background command.

use std::time::Duration;

use harn_vm::{VmDictExt, VmValue};
use regex_automata::{hybrid::dfa::DFA, hybrid::LazyStateID, Input};

use crate::error::HostlibError;
use crate::tools::payload::{
    optional_bool, optional_string, optional_u64, require_dict_arg, require_string,
};

pub(crate) const NAME: &str = "hostlib_tools_wait_command_output";

#[derive(Clone, Copy)]
enum Source {
    Stdout,
    Stderr,
    Combined,
}

impl Source {
    fn parse(value: Option<String>) -> Result<Self, HostlibError> {
        match value.as_deref().unwrap_or("combined") {
            "stdout" => Ok(Self::Stdout),
            "stderr" => Ok(Self::Stderr),
            "combined" => Ok(Self::Combined),
            _ => Err(HostlibError::InvalidParameter {
                builtin: NAME,
                param: "source",
                message: "expected stdout, stderr, or combined".to_string(),
            }),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
            Self::Combined => "combined",
        }
    }

    fn bytes(self, state: &super::long_running::OutputState) -> &[u8] {
        match self {
            Self::Stdout => &state.stdout,
            Self::Stderr => &state.stderr,
            Self::Combined => &state.combined,
        }
    }
}

enum Matcher {
    Literal(LiteralMatcher),
    Regex(Box<RegexMatcher>),
}

impl Matcher {
    fn new(pattern: &str, regex: bool, from_offset: usize) -> Result<Self, HostlibError> {
        if pattern.is_empty() {
            return Err(HostlibError::InvalidParameter {
                builtin: NAME,
                param: "pattern",
                message: "must not be empty".to_string(),
            });
        }
        if regex {
            RegexMatcher::new(pattern, from_offset).map(|matcher| Self::Regex(Box::new(matcher)))
        } else {
            Ok(Self::Literal(LiteralMatcher::new(
                pattern.as_bytes(),
                from_offset,
            )))
        }
    }

    fn find(&mut self, bytes: &[u8]) -> Result<Option<(usize, usize)>, HostlibError> {
        match self {
            Self::Literal(matcher) => Ok(matcher.find(bytes)),
            Self::Regex(matcher) => matcher.find(bytes),
        }
    }
}

struct LiteralMatcher {
    pattern: Vec<u8>,
    prefix: Vec<usize>,
    matched: usize,
    processed: usize,
}

impl LiteralMatcher {
    fn new(pattern: &[u8], from_offset: usize) -> Self {
        let mut prefix = vec![0; pattern.len()];
        let mut matched = 0;
        for index in 1..pattern.len() {
            while matched > 0 && pattern[index] != pattern[matched] {
                matched = prefix[matched - 1];
            }
            if pattern[index] == pattern[matched] {
                matched += 1;
            }
            prefix[index] = matched;
        }
        Self {
            pattern: pattern.to_vec(),
            prefix,
            matched: 0,
            processed: from_offset,
        }
    }

    fn find(&mut self, bytes: &[u8]) -> Option<(usize, usize)> {
        while self.processed < bytes.len() {
            let byte = bytes[self.processed];
            while self.matched > 0 && byte != self.pattern[self.matched] {
                self.matched = self.prefix[self.matched - 1];
            }
            if byte == self.pattern[self.matched] {
                self.matched += 1;
            }
            self.processed += 1;
            if self.matched == self.pattern.len() {
                return Some((self.processed - self.pattern.len(), self.processed));
            }
        }
        None
    }
}

struct RegexMatcher {
    dfa: DFA,
    cache: regex_automata::hybrid::dfa::Cache,
    state: Option<LazyStateID>,
    finder: regex::bytes::Regex,
    from_offset: usize,
    processed: usize,
}

impl RegexMatcher {
    fn new(pattern: &str, from_offset: usize) -> Result<Self, HostlibError> {
        let dfa = DFA::new(pattern).map_err(|error| HostlibError::InvalidParameter {
            builtin: NAME,
            param: "pattern",
            message: error.to_string(),
        })?;
        let finder =
            regex::bytes::Regex::new(pattern).map_err(|error| HostlibError::InvalidParameter {
                builtin: NAME,
                param: "pattern",
                message: error.to_string(),
            })?;
        let cache = dfa.create_cache();
        Ok(Self {
            dfa,
            cache,
            state: None,
            finder,
            from_offset,
            processed: from_offset,
        })
    }

    fn find(&mut self, bytes: &[u8]) -> Result<Option<(usize, usize)>, HostlibError> {
        if self.from_offset > bytes.len() {
            return Ok(None);
        }
        if self.state.is_none() {
            self.state = Some(
                self.dfa
                    .start_state_forward(
                        &mut self.cache,
                        &Input::new(bytes).span(self.from_offset..bytes.len()),
                    )
                    .map_err(|error| backend(error.to_string()))?,
            );
        }
        let mut state = self.state.expect("regex state initialized");
        while self.processed < bytes.len() {
            let index = self.processed;
            state = self
                .dfa
                .next_state(&mut self.cache, state, bytes[index])
                .map_err(|error| backend(error.to_string()))?;
            self.processed += 1;
            if state.is_match() {
                self.state = Some(state);
                return Ok(self.exact_match(bytes, index));
            }
        }
        self.state = Some(state);
        let end_state = self
            .dfa
            .next_eoi_state(&mut self.cache, state)
            .map_err(|error| backend(error.to_string()))?;
        if end_state.is_match() {
            return Ok(self.exact_match(bytes, bytes.len()));
        }
        Ok(None)
    }

    fn exact_match(&self, bytes: &[u8], end: usize) -> Option<(usize, usize)> {
        self.finder
            .find(&bytes[self.from_offset..end])
            .map(|found| {
                (
                    self.from_offset + found.start(),
                    self.from_offset + found.end(),
                )
            })
    }
}

fn backend(message: String) -> HostlibError {
    HostlibError::Backend {
        builtin: NAME,
        message,
    }
}

pub(crate) async fn handle(args: Vec<VmValue>) -> Result<VmValue, HostlibError> {
    let map = require_dict_arg(NAME, &args)?;
    let handle_id = require_string(NAME, &map, "handle_id")?;
    let pattern = require_string(NAME, &map, "pattern")?;
    let source = Source::parse(optional_string(NAME, &map, "source")?)?;
    let regex = optional_bool(NAME, &map, "regex")?.unwrap_or(false);
    let from_offset = usize::try_from(optional_u64(NAME, &map, "from_offset")?.unwrap_or(0))
        .map_err(|_| HostlibError::InvalidParameter {
            builtin: NAME,
            param: "from_offset",
            message: "offset exceeds this platform's addressable output size".to_string(),
        })?;
    let timeout_ms = optional_u64(NAME, &map, "timeout_ms")?;
    let session_id = optional_string(NAME, &map, "session_id")?
        .or_else(harn_vm::current_agent_session_id)
        .unwrap_or_default();
    let mut matcher = Matcher::new(&pattern, regex, from_offset)?;

    if let Some(feed) = super::long_running::output_feed_for_handle(&handle_id) {
        let timeout = async move {
            match timeout_ms {
                Some(ms) => tokio::time::sleep(Duration::from_millis(ms)).await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::pin!(timeout);
        loop {
            let notified = feed.notified();
            tokio::pin!(notified);
            // `notify_waiters` does not retain a permit for an unregistered
            // future. Register before inspecting state so publication cannot
            // land in the check/select gap.
            notified.as_mut().enable();
            let ready = {
                let state = feed
                    .state
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner());
                let bytes = source.bytes(&state);
                if let Some((start, end)) = matcher.find(bytes)? {
                    Some((
                        response(
                            "matched",
                            &handle_id,
                            source,
                            &pattern,
                            Some((start, end, &bytes[start..end])),
                            running_result(&handle_id, &state),
                        ),
                        false,
                    ))
                } else {
                    state.terminal.clone().map(|result| {
                        (
                            response("exited", &handle_id, source, &pattern, None, result),
                            true,
                        )
                    })
                }
            };
            if let Some((result, drain_terminal)) = ready {
                if drain_terminal {
                    let _ = super::wait_command::drain_matching_result(&session_id, &handle_id);
                }
                return Ok(result);
            }
            tokio::select! {
                biased;
                () = &mut notified => {}
                () = &mut timeout => {
                    return Ok(response(
                        "timed_out",
                        &handle_id,
                        source,
                        &pattern,
                        None,
                        running_result_from_feed(&handle_id, &feed),
                    ));
                }
            }
        }
    }

    if let Some(result) = super::wait_command::drain_matching_result(&session_id, &handle_id) {
        return Ok(response(
            "exited", &handle_id, source, &pattern, None, result,
        ));
    }
    Err(HostlibError::InvalidParameter {
        builtin: NAME,
        param: "handle_id",
        message: format!("unknown background command handle `{handle_id}`"),
    })
}

fn running_result_from_feed(handle_id: &str, feed: &super::long_running::OutputFeed) -> VmValue {
    let state = feed
        .state
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    running_result(handle_id, &state)
}

fn running_result(handle_id: &str, state: &super::long_running::OutputState) -> VmValue {
    let mut result = harn_vm::value::DictMap::new();
    result.put_str("handle_id", handle_id);
    result.put_str("status", "running");
    result.insert("completed".into(), VmValue::Bool(false));
    result.insert("timed_out".into(), VmValue::Bool(false));
    result.insert("exit_code".into(), VmValue::Nil);
    result.put_str("stdout", String::from_utf8_lossy(&state.stdout));
    result.put_str("stderr", String::from_utf8_lossy(&state.stderr));
    result.put_str("combined", String::from_utf8_lossy(&state.combined));
    result.insert(
        "byte_count".into(),
        VmValue::Int(state.combined.len() as i64),
    );
    VmValue::dict(result)
}

fn response(
    status: &str,
    handle_id: &str,
    source: Source,
    pattern: &str,
    found: Option<(usize, usize, &[u8])>,
    result: VmValue,
) -> VmValue {
    let mut response = harn_vm::value::DictMap::new();
    response.put_str("status", status);
    response.put_str("handle_id", handle_id);
    response.put_str("source", source.as_str());
    response.put_str("pattern", pattern);
    response.insert("matched".into(), VmValue::Bool(found.is_some()));
    response.insert("timed_out".into(), VmValue::Bool(status == "timed_out"));
    if let Some((start, end, bytes)) = found {
        response.insert("match_start".into(), VmValue::Int(start as i64));
        response.insert("match_end".into(), VmValue::Int(end as i64));
        response.insert("next_offset".into(), VmValue::Int(end as i64));
        response.put_str("match", String::from_utf8_lossy(bytes));
    } else {
        for key in ["match_start", "match_end", "next_offset", "match"] {
            response.insert(key.into(), VmValue::Nil);
        }
    }
    response.insert("result".into(), result);
    VmValue::dict(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_matcher_carries_state_across_chunks() {
        let mut matcher = Matcher::new("ready", false, 2).unwrap();
        assert_eq!(matcher.find(b"xxre").unwrap(), None);
        assert_eq!(matcher.find(b"xxready now").unwrap(), Some((2, 7)));
    }

    #[test]
    fn regex_matcher_carries_state_across_chunks() {
        let mut matcher = Matcher::new(r"ready\s+on\s+\d+", true, 0).unwrap();
        assert_eq!(matcher.find(b"ready on").unwrap(), None);
        assert_eq!(matcher.find(b"ready on 4312").unwrap(), Some((0, 10)));
    }
}
