//! Restored postings, with tombstones and a delta layered over the shared
//! base, must answer exactly like postings that were edited live.

use crate::code_index::trigram::{query_trigrams, TrigramIndex};
use crate::code_index::words::WordIndex;

const FILES: [(u32, &str); 3] = [
    (1, "fn alpha_value() { shared_name(); }\n"),
    (2, "struct Beta;\nfn shared_name() { alpha_value(); }\n"),
    (3, "fn gamma() { shared_name(); shared_name(); }\n"),
];

const PROBES: [&str; 6] = [
    "alpha_value",
    "shared_name",
    "gamma",
    "Beta",
    "delta_only",
    "replaced",
];

fn seeded() -> (TrigramIndex, WordIndex) {
    let (mut trigrams, mut words) = (TrigramIndex::new(), WordIndex::new());
    for (id, source) in FILES {
        trigrams.index_file(id, source);
        words.index_file(id, source);
    }
    (trigrams, words)
}

fn restored(trigrams: &TrigramIndex, words: &WordIndex) -> (TrigramIndex, WordIndex) {
    (
        TrigramIndex::from_postings(trigrams.snapshot_postings()),
        WordIndex::from_postings(words.snapshot_postings()),
    )
}

fn edit(trigrams: &mut TrigramIndex, words: &mut WordIndex) {
    let replaced = "fn replaced() { delta_only(); }\n";
    trigrams.index_file(2, replaced);
    words.index_file(2, replaced);
    trigrams.remove_file(3);
    words.remove_file(3);
    let added = "fn delta_only() { alpha_value(); }\n";
    trigrams.index_file(4, added);
    words.index_file(4, added);
}

/// Every probe's word hits and trigram candidates, plus both snapshots.
fn view(trigrams: &TrigramIndex, words: &WordIndex) -> String {
    let probes: Vec<_> = PROBES
        .iter()
        .map(|probe| {
            let hits: Vec<_> = words.get(probe).iter().map(|h| (h.file, h.line)).collect();
            let mut files: Vec<_> = trigrams.query(&query_trigrams(probe)).into_iter().collect();
            files.sort_unstable();
            (probe, hits, files)
        })
        .collect();
    let trigram_rows: Vec<_> = trigrams
        .snapshot_postings()
        .into_iter()
        .map(|p| (p.trigram, p.files))
        .collect();
    let word_rows: Vec<_> = words
        .snapshot_postings()
        .into_iter()
        .map(|p| (p.word, p.hits))
        .collect();
    format!(
        "{probes:?}\n{trigram_rows:?}\n{word_rows:?}\n{} {}",
        trigrams.distinct_trigrams(),
        words.distinct_words()
    )
}

#[test]
fn restored_postings_answer_like_live_ones_before_and_after_edits() {
    let (mut live_t, mut live_w) = seeded();
    let (mut restored_t, mut restored_w) = restored(&live_t, &live_w);
    assert_eq!(view(&restored_t, &restored_w), view(&live_t, &live_w));
    edit(&mut live_t, &mut live_w);
    edit(&mut restored_t, &mut restored_w);
    assert_eq!(view(&restored_t, &restored_w), view(&live_t, &live_w));
    // The negative control: the edits changed what the view reports.
    let (seed_t, seed_w) = seeded();
    assert_ne!(view(&live_t, &live_w), view(&seed_t, &seed_w));
    assert!(
        restored_w.get("gamma").is_empty(),
        "a removed file's hits are gone"
    );
}

#[test]
fn memory_estimate_tracks_live_content_across_restore_and_edits() {
    let (mut live_t, mut live_w) = seeded();
    let (mut restored_t, mut restored_w) = restored(&live_t, &live_w);
    let estimate = |t: &TrigramIndex, w: &WordIndex| (t.estimated_bytes(), w.estimated_bytes());

    let original = estimate(&live_t, &live_w);
    assert!(original.0 > 0 && original.1 > 0);
    assert_eq!(estimate(&restored_t, &restored_w), original);

    edit(&mut live_t, &mut live_w);
    edit(&mut restored_t, &mut restored_w);
    let edited = estimate(&live_t, &live_w);
    assert_ne!(edited, original, "changed content must affect the estimate");
    assert_eq!(estimate(&restored_t, &restored_w), edited);
    restored_t.postings.compact();
    restored_w.postings.compact();
    assert_eq!(estimate(&restored_t, &restored_w), edited);
}

#[test]
fn memory_estimate_does_not_clone_live_hits_after_edit() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    struct Hit {
        file: u32,
        clones: Arc<AtomicUsize>,
    }
    impl Clone for Hit {
        fn clone(&self) -> Self {
            self.clones.fetch_add(1, Ordering::Relaxed);
            Self {
                file: self.file,
                clones: Arc::clone(&self.clones),
            }
        }
    }
    impl super::FilePosting for Hit {
        fn file(&self) -> u32 {
            self.file
        }
    }

    let clones = Arc::new(AtomicUsize::new(0));
    let hit = |file| Hit {
        file,
        clones: Arc::clone(&clones),
    };
    let mut postings =
        super::FilePostings::from_files([(1, vec![(7_u8, hit(1))]), (2, vec![(7_u8, hit(2))])]);
    postings.replace_file(1, [(7_u8, vec![hit(1)])]);
    clones.store(0, Ordering::Relaxed);

    let bytes = postings.estimated_bytes(|_| 0);
    assert_eq!(
        bytes,
        std::mem::size_of::<u8>()
            + std::mem::size_of::<Vec<Hit>>()
            + 2 * std::mem::size_of::<Hit>()
    );
    assert_eq!(clones.load(Ordering::Relaxed), 0);
}

#[test]
fn compaction_preserves_every_read() {
    let (seed_t, seed_w) = seeded();
    let (mut trigrams, mut words) = restored(&seed_t, &seed_w);
    edit(&mut trigrams, &mut words);
    let before = view(&trigrams, &words);
    trigrams.postings.compact();
    words.postings.compact();
    assert_eq!(view(&trigrams, &words), before);
    // A file folded into the new base can still be replaced and removed.
    let (mut live_t, mut live_w) = seeded();
    edit(&mut live_t, &mut live_w);
    for (t, w) in [(&mut trigrams, &mut words), (&mut live_t, &mut live_w)] {
        t.remove_file(4);
        w.remove_file(4);
        t.index_file(1, "fn alpha_value() {}\n");
        w.index_file(1, "fn alpha_value() {}\n");
    }
    assert_eq!(view(&trigrams, &words), view(&live_t, &live_w));
}

#[test]
fn clones_share_the_base_without_seeing_each_others_edits() {
    let (seed_t, seed_w) = seeded();
    let (trigrams, words) = restored(&seed_t, &seed_w);
    let before = view(&trigrams, &words);
    let (mut branch_t, mut branch_w) = (trigrams.clone(), words.clone());
    edit(&mut branch_t, &mut branch_w);
    assert_eq!(view(&trigrams, &words), before);
    assert_ne!(view(&branch_t, &branch_w), before);
}
