use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BatchSize, Criterion};
use ed25519_dalek::SigningKey;
use harn_vm::flow::{Atom, Provenance, SqliteFlowStore, TextOp};
use time::OffsetDateTime;

fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn build_atoms(count: usize) -> Vec<Atom> {
    let principal = key(1);
    let persona = key(2);
    let mut atoms = Vec::with_capacity(count);
    for index in 0..count {
        let parents = atoms
            .last()
            .map(|atom: &Atom| vec![atom.id])
            .unwrap_or_default();
        let timestamp = OffsetDateTime::from_unix_timestamp(1_775_000_000 + index as i64).unwrap();
        atoms.push(
            Atom::sign(
                vec![TextOp::Insert {
                    offset: index as u64,
                    content: format!("atom-{index}"),
                }],
                parents,
                Provenance {
                    principal: "user:bench".to_string(),
                    persona: "ship-captain".to_string(),
                    agent_run_id: format!("run-{index}"),
                    tool_call_id: Some(format!("tool-{index}")),
                    trace_id: "trace-bench".to_string(),
                    transcript_ref: "transcript-bench".to_string(),
                    timestamp,
                },
                None,
                &principal,
                &persona,
            )
            .unwrap(),
        );
    }
    atoms
}

fn ten_thousand_atom_dag_round_trip(c: &mut Criterion) {
    let atoms = build_atoms(10_000);
    c.bench_function("flow_store_10k_atom_dag_round_trip", |b| {
        b.iter_batched(
            || SqliteFlowStore::in_memory("bench-site").unwrap(),
            |store| {
                store.emit_preverified_atoms(black_box(&atoms)).unwrap();
                let queried = store
                    .atom_count_for_principal_persona("user:bench", "ship-captain")
                    .unwrap();
                black_box(queried)
            },
            BatchSize::SmallInput,
        );
    });
}

criterion_group!(benches, ten_thousand_atom_dag_round_trip);
criterion_main!(benches);
