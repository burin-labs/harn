//! Stable process-shard assignment for conformance files.

use std::path::PathBuf;

use crate::test_runner::TestShard;

pub(super) fn select_conformance_shard(
    items: Vec<(PathBuf, String)>,
    shard: Option<TestShard>,
) -> Vec<(PathBuf, String)> {
    let Some(shard) = shard else {
        return items;
    };
    items
        .into_iter()
        .filter(|(_, relative_path)| {
            let digest = blake3::hash(relative_path.as_bytes());
            let mut prefix = [0_u8; 8];
            prefix.copy_from_slice(&digest.as_bytes()[..8]);
            u64::from_le_bytes(prefix) % shard.total() as u64 == (shard.index() - 1) as u64
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::*;

    fn suite(size: usize) -> Vec<(PathBuf, String)> {
        (0..size)
            .map(|index| {
                (
                    PathBuf::from(format!("case-{index}.harn")),
                    format!("case-{index}.harn"),
                )
            })
            .collect()
    }

    fn assignment(items: Vec<(PathBuf, String)>, total: usize) -> BTreeMap<String, usize> {
        (1..=total)
            .flat_map(|index| {
                select_conformance_shard(items.clone(), Some(TestShard::new(index, total).unwrap()))
                    .into_iter()
                    .map(move |(_, path)| (path, index))
            })
            .collect()
    }

    #[test]
    fn shards_are_disjoint_and_cover_the_suite() {
        let suite = suite(10);
        let assigned = (1..=3)
            .flat_map(|index| {
                select_conformance_shard(suite.clone(), Some(TestShard::new(index, 3).unwrap()))
            })
            .map(|(_, path)| path)
            .collect::<Vec<_>>();
        let unique = assigned.iter().cloned().collect::<BTreeSet<_>>();
        let expected = suite
            .iter()
            .map(|(_, path)| path.clone())
            .collect::<BTreeSet<_>>();

        assert_eq!(assigned.len(), suite.len());
        assert_eq!(unique, expected);
    }

    #[test]
    fn adding_a_case_does_not_move_existing_assignments() {
        let suite = suite(20);
        let before = assignment(suite.clone(), 4);
        let mut expanded = suite;
        expanded.push((PathBuf::from("new-case.harn"), "new-case.harn".to_string()));
        let after = assignment(expanded, 4);

        for (path, shard) in before {
            assert_eq!(after.get(&path), Some(&shard), "{path} moved shards");
        }
    }
}
