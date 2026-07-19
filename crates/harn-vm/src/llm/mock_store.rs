//! Per-scope queues for versioned LLM mock fixtures.
//!
//! The parser owns document validation; this module owns the mutable queue
//! invariants shared by builtin and CLI replay. Keeping matching here prevents
//! the two installation paths from growing subtly different fallback rules.

use std::collections::{BTreeMap, VecDeque};

use harn_glob::match_prose as mock_glob_match;

use super::mock::{LlmMock, LlmMockFixture, MockConsumptionReceipt, DEFAULT_MOCK_SCOPE};

/// A mutable fixture queue partitioned by the logical scope requested by an
/// LLM call. The map retains empty buckets so snapshots can report a scope
/// whose `once` entries have all been consumed.
#[derive(Clone, Debug, Default)]
pub(crate) struct MockQueue {
    schema_version: u32,
    strict_scopes: bool,
    buckets: BTreeMap<String, VecDeque<LlmMock>>,
    warnings: Vec<String>,
}

/// A response selected by the queue matcher, with the receipt constructed at
/// the same mutation boundary as the once/sticky consumption decision.
pub(crate) struct QueueMatch {
    pub mock: LlmMock,
    pub receipt: MockConsumptionReceipt,
}

impl MockQueue {
    pub(crate) const fn new() -> Self {
        Self {
            schema_version: 0,
            strict_scopes: false,
            buckets: BTreeMap::new(),
            warnings: Vec::new(),
        }
    }

    pub(crate) fn from_fixture(fixture: LlmMockFixture) -> Self {
        let mut queue = Self {
            schema_version: fixture.schema_version,
            strict_scopes: fixture.strict_scopes,
            buckets: BTreeMap::new(),
            warnings: fixture.warnings,
        };
        for mock in fixture.mocks {
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

    pub(crate) fn is_active(&self) -> bool {
        self.schema_version > 0 || self.count() > 0
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
            });
        }

        if requested_scope != DEFAULT_MOCK_SCOPE && !self.strict_scopes {
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
                });
            }
        }

        None
    }

    pub(crate) fn push_v0(&mut self, mut mock: LlmMock) {
        mock.scope = DEFAULT_MOCK_SCOPE.to_string();
        self.schema_version = 0;
        self.strict_scopes = false;
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
