"""Run a source-pinned finite algebra proof with the already-installed Z3.

This does not execute or verify Rust. It refuses changed production source so
that a successful old abstraction cannot silently qualify a newer validator.
The companion Rust integration test is the missing refinement check until run
against the production crate on an admitted host / normal CI.
"""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess

SOURCE = 'crates/harn-vm/src/actor_chain.rs'
BASE = '507ae13d0978244ff45a8520cd04ec6263c31cc0'
SOURCE_SHA256 = '7b283a3f6cbc0aae652de1b4b2a55cb66c036fb391aa04bc44af43dae05d47bf'
EXPECTED = [
    ('non_increasing_iff_membership', 'unsat'),
    ('strict_iff_proper_subset', 'unsat'),
    ('non_increasing_chain_never_widens_origin', 'unsat'),
    ('strict_chain_never_widens_origin', 'unsat'),
    ('strict_chain_consumes_at_least_one_scope_per_hop', 'unsat'),
    ('strict_rejects_equal_including_empty', 'unsat'),
    ('empty_parent_cannot_delegate_nonempty_child', 'unsat'),
    ('empty_child_is_non_increasing', 'unsat'),
    ('widening_is_reachable_for_off_mode', 'sat'),
    ('negative_control_drop_subset_guard', 'sat'),
    ('negative_control_drop_strictness', 'sat'),
    ('negative_control_skip_origin_edge', 'sat'),
]


def run(args):
    return subprocess.run(args, check=True, capture_output=True, text=True, timeout=60)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repo', type=Path, required=True)
    parser.add_argument('--ref', default='HEAD')
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    root = Path(__file__).resolve().parent
    source = subprocess.run(
        ['git', '-C', str(args.repo), 'show', f'{args.ref}:{SOURCE}'],
        check=True, capture_output=True, timeout=20,
    ).stdout
    digest = hashlib.sha256(source).hexdigest()
    if digest != SOURCE_SHA256:
        raise SystemExit(f'SOURCE DRIFT: expected {SOURCE_SHA256}; got {digest}; no proof run')
    smt = root / 'scope_algebra.smt2'
    solved = run(['z3', '-smt2', str(smt)])
    if solved.stderr or '(error' in solved.stdout:
        raise SystemExit(f'Z3 error: {solved.stderr}\n{solved.stdout}')
    lines = solved.stdout.splitlines()
    queries = [line.strip('"') for line in lines if line.strip('"') in dict(EXPECTED)]
    results = [line for line in lines if line in {'sat', 'unsat', 'unknown'}]
    if queries != [name for name, _ in EXPECTED] or results != [status for _, status in EXPECTED]:
        raise SystemExit(f'Unexpected/missing solver verdicts:\n{solved.stdout}')
    args.out.mkdir(parents=True, exist_ok=True)
    (args.out / 'z3.txt').write_text(solved.stdout)
    record = {
        'claim': 'finite set abstraction checked; production Rust test NOT RUN',
        'source_commit': run(['git', '-C', str(args.repo), 'rev-parse', args.ref]).stdout.strip(),
        'source_path': SOURCE,
        'source_sha256': digest,
        'solver': run(['z3', '--version']).stdout.strip(),
        'smt_sha256': hashlib.sha256(smt.read_bytes()).hexdigest(),
        'bounds': {'scope_symbols': 4, 'delegated_actors': [0, 1, 2, 3]},
        'verdicts': dict(EXPECTED),
        'raw_output': 'z3.txt',
        'trusted': ['Z3', 'SMT encoding', 'BTreeSet-to-bitset refinement', 'Rust loop-to-chain refinement'],
        'rust_execution': 'pending admitted host or normal protected CI',
        'independent_review': 'pending',
    }
    (args.out / 'result.json').write_text(json.dumps(record, indent=2) + '\n')
    print(json.dumps(record, indent=2))


if __name__ == '__main__':
    main()
