//! Per-scope queues for versioned LLM mock fixtures.
//!
//! The parser owns document validation; this module owns the mutable queue
//! invariants shared by builtin and CLI replay. Keeping matching here prevents
//! the two installation paths from growing subtly different fallback rules.

use std::collections::{BTreeMap, VecDeque};

use harn_glob::match_prose as mock_glob_match;

use super::mock::{
    LlmMock, LlmMockFixture, LlmMockPrefixMarker, LlmMockPrefixServedBy, MockConsumptionReceipt,
    DEFAULT_MOCK_SCOPE, SHARED_MOCK_SCOPE,
};

/// A mutable fixture queue partitioned by the logical scope requested by an
/// LLM call. The map retains empty buckets so snapshots can report a scope
/// whose `once` entries have all been consumed.
#[derive(Clone, Debug, Default)]
pub(crate) struct MockQueue {
    schema_version: u32,
    strict_scopes: bool,
    buckets: BTreeMap<String, VecDeque<LlmMock>>,
    warnings: Vec<String>,
    live_prefix: Option<LivePrefix>,
}

/// Progress through a `liveAfterCalls` fixture. `calls` counts every call this
/// queue answered or handed off, so the fixture/live boundary and each call's
/// ordinal come from one counter under the queue's own lock.
#[derive(Clone, Copy, Debug)]
struct LivePrefix {
    live_after_calls: u64,
    calls: u64,
}

impl LivePrefix {
    fn handed_off(self) -> bool {
        self.calls >= self.live_after_calls
    }

    fn next(&mut self, served_by: LlmMockPrefixServedBy) -> LlmMockPrefixMarker {
        self.calls += 1;
        LlmMockPrefixMarker {
            served_by,
            call: self.calls,
            live_after_calls: self.live_after_calls,
        }
    }
}

/// A response selected by the queue matcher, with the receipt constructed at
/// the same mutation boundary as the once/sticky consumption decision.
pub(crate) struct QueueMatch {
    pub mock: LlmMock,
    pub receipt: MockConsumptionReceipt,
    /// Present only for a `liveAfterCalls` fixture.
    pub prefix: Option<LlmMockPrefixMarker>,
}

impl MockQueue {
    pub(crate) fn from_fixture(fixture: LlmMockFixture) -> Self {
        let mut queue = Self {
            schema_version: fixture.schema_version,
            strict_scopes: fixture.strict_scopes,
            buckets: BTreeMap::new(),
            warnings: fixture.warnings,
            live_prefix: fixture.live_after_calls.map(|live_after_calls| LivePrefix {
                live_after_calls,
                calls: 0,
            }),
        };
        let mut mocks = fixture.mocks;
        // Entries past the prefix are never served: a full recording can carry
        // a shorter prefix without being truncated by hand.
        if let Some(live_after_calls) = fixture.live_after_calls {
            mocks.truncate(usize::try_from(live_after_calls).unwrap_or(usize::MAX));
        }
        for mock in mocks {
            queue
                .buckets
                .entry(mock.scope.clone())
                .or_default()
                .push_back(mock);
        }
        queue
    }

    pub(crate) fn schema_version(&self) -> u32 {
        self.schema_version
    }

    pub(crate) fn strict_scopes(&self) -> bool {
        self.strict_scopes
    }

    pub(crate) fn count(&self) -> usize {
        self.buckets.values().map(VecDeque::len).sum()
    }

    /// Whether this queue still intercepts calls. A `liveAfterCalls` fixture
    /// stops intercepting once its prefix is served, which is what routes the
    /// next call through the configured provider.
    pub(crate) fn is_active(&self) -> bool {
        !self.handed_off() && (self.schema_version > 0 || self.count() > 0)
    }

    pub(crate) fn live_after_calls(&self) -> Option<u64> {
        self.live_prefix.map(|prefix| prefix.live_after_calls)
    }

    /// True once a `liveAfterCalls` prefix has served all of its calls.
    pub(crate) fn handed_off(&self) -> bool {
        self.live_prefix.is_some_and(LivePrefix::handed_off)
    }

    /// The call a miss would have been inside an unfinished replay prefix, as
    /// `(call, live_after_calls)`. A miss there is a divergence from the
    /// recording and must fail closed instead of going live early.
    pub(crate) fn prefix_miss(&self) -> Option<(u64, u64)> {
        self.live_prefix
            .filter(|prefix| !prefix.handed_off())
            .map(|prefix| (prefix.calls + 1, prefix.live_after_calls))
    }

    /// Count one live call after the handoff and return its marker.
    pub(crate) fn next_live_call(&mut self) -> Option<LlmMockPrefixMarker> {
        let prefix = self
            .live_prefix
            .as_mut()
            .filter(|prefix| prefix.handed_off())?;
        Some(prefix.next(LlmMockPrefixServedBy::Live))
    }

    pub(crate) fn scopes(&self) -> Vec<String> {
        self.buckets.keys().cloned().collect()
    }

    pub(crate) fn queue_remaining(&self) -> BTreeMap<String, usize> {
        self.buckets
            .iter()
            .map(|(scope, queue)| (scope.clone(), queue.len()))
            .collect()
    }

    pub(crate) fn warnings(&self) -> &[String] {
        &self.warnings
    }

    pub(crate) fn match_request(
        &mut self,
        requested_scope: &str,
        match_text: &str,
    ) -> Option<QueueMatch> {
        if self.handed_off() {
            return None;
        }
        let mut selected = self.match_scopes(requested_scope, match_text)?;
        selected.prefix = self
            .live_prefix
            .as_mut()
            .map(|prefix| prefix.next(LlmMockPrefixServedBy::Fixture));
        Some(selected)
    }

    fn match_scopes(&mut self, requested_scope: &str, match_text: &str) -> Option<QueueMatch> {
        if let Some((mock, remaining)) = self.match_bucket(requested_scope, match_text) {
            return Some(QueueMatch {
                receipt: MockConsumptionReceipt::hit(
                    requested_scope,
                    requested_scope,
                    &mock,
                    false,
                    remaining,
                ),
                mock,
                prefix: None,
            });
        }

        if requested_scope != DEFAULT_MOCK_SCOPE && requested_scope != SHARED_MOCK_SCOPE {
            if let Some((mock, remaining)) = self.match_bucket(SHARED_MOCK_SCOPE, match_text) {
                return Some(QueueMatch {
                    receipt: MockConsumptionReceipt::hit(
                        requested_scope,
                        SHARED_MOCK_SCOPE,
                        &mock,
                        true,
                        remaining,
                    ),
                    mock,
                    prefix: None,
                });
            }
        }

        if requested_scope != DEFAULT_MOCK_SCOPE
            && requested_scope != SHARED_MOCK_SCOPE
            && !self.strict_scopes
        {
            if let Some((mock, remaining)) = self.match_bucket(DEFAULT_MOCK_SCOPE, match_text) {
                return Some(QueueMatch {
                    receipt: MockConsumptionReceipt::hit(
                        requested_scope,
                        DEFAULT_MOCK_SCOPE,
                        &mock,
                        true,
                        remaining,
                    ),
                    mock,
                    prefix: None,
                });
            }
        }

        None
    }

    pub(crate) fn push_v0(&mut self, mut mock: LlmMock) {
        mock.scope = DEFAULT_MOCK_SCOPE.to_string();
        self.schema_version = 0;
        self.strict_scopes = false;
        self.live_prefix = None;
        self.warnings.clear();
        self.buckets
            .entry(DEFAULT_MOCK_SCOPE.to_string())
            .or_default()
            .push_back(mock);
    }

    pub(crate) fn miss_receipt(&self, requested_scope: &str) -> MockConsumptionReceipt {
        MockConsumptionReceipt::miss(
            requested_scope,
            self.buckets.get(requested_scope).map_or(0, VecDeque::len),
        )
    }

    fn match_bucket(&mut self, scope: &str, match_text: &str) -> Option<(LlmMock, usize)> {
        let queue = self.buckets.get_mut(scope)?;
        let index = queue
            .iter()
            .position(|mock| mock.match_pattern.is_none())
            .or_else(|| {
                queue.iter().position(|mock| {
                    mock.match_pattern
                        .as_ref()
                        .is_some_and(|pattern| mock_glob_match(pattern, match_text))
                })
            })?;

        let mock = if queue[index].sticky {
            queue[index].clone()
        } else {
            queue.remove(index)?
        };
        Some((mock, queue.len()))
    }
}

#[cfg(test)]
#[path = "mock_store_tests.rs"]
mod tests;
