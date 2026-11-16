# Consumer composition reference

A consumer composition selects one consumer pull request that contains every
adaptation required by a Harn source. CI and release rehearsals test that
combined source. The ordinary [consumer observer](consumer-observer.md) still
owns dispatch, custody, product decisions, and cleanup.

## Source declaration

The included Harn source declares every adaptation and one composition:

```text
Pairs-with: consumer#7
Pairs-with: consumer#8
Consumer-composition: consumer#9
```

The composition number must differ from every adaptation number. Repeated
identical declarations are accepted. Distinct composition numbers, malformed
trailers, or a composition without adaptations refuse selection. An explicit
consumer pull input must equal the composition number.

An ordinary source with no pairing selects the consumer's default branch.
One pairing selects its pull request. Multiple pairings without a composition
still refuse. Release selection reads every included commit after the previous
stable tag through the exact source revision.

## Manifest

The composition's final commit changes only
`.github/harn-consumer-composition.json`. Its closed JSON object has this shape:

```json
{
  "schema": "harn.consumer_composition.v1",
  "repository": "owner/consumer",
  "base_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  "adaptations": [
    {"pull_request": 7, "head_sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"},
    {"pull_request": 8, "head_sha": "cccccccccccccccccccccccccccccccccccccccc"}
  ],
  "repairs": [
    {"pull_request": 12, "head_sha": "dddddddddddddddddddddddddddddddddddddddd"}
  ]
}
```

`repository` must equal the authenticated consumer repository. `base_sha` and
each `head_sha` contain 40 lowercase hexadecimal characters. Pull numbers are
positive safe JSON integers. Unknown fields refuse decoding.

`adaptations` must cover exactly the source's pairing numbers. `repairs` may add
separate integration fixes; they can't replace an adaptation. The two arrays
must contain distinct pull numbers and exclude the composition itself. Their
combined limit is 64 components. The manifest limit is 32 KiB.

## Authenticated graph

The reader retrieves the manifest at the composition's exact head through the
[repository contents API](https://docs.github.com/en/rest/repos/contents#get-repository-content).
It checks the decoded byte count, UTF-8, Base64, and Git blob identity.

The composition must remain open in the consumer repository and target its
default branch. Every component must belong to that repository, target the
default branch, and retain its declared head. Components may be open or merged;
closed unmerged components refuse.

The declared base must be an ancestor of both the current default head and the
composition head. Every component head must be an ancestor of the composition
head. The reader measures complete commit inventories through GitHub's
[paginated comparison API](https://docs.github.com/en/rest/commits/commits#compare-two-commits).
Each comparison is limited to 1,000 commits; a resolution is limited to 256 reads.

The composition may contain component history, merge commits, and one final
linear commit for the manifest. Additional linear code commits refuse.
Conflict resolutions belong in the corresponding merge commits and receive
normal pull request review. Ancestry alone doesn't prove those resolutions
preserve product behavior; the combined rehearsal supplies that evidence.

The reader refreshes every component and the composition after measuring the
graph. Observation and publication repeat source qualification before accepting
the rehearsed result. Missing reads, partial inventories, and moved identities
refuse qualification.

## Release binding and privacy

Ordinary rehearsals retain `harn.release.consumer_rehearsal_binding.v1`.
Composition rehearsals use version 2 and add a closed `composition` identity:

```json
{
  "schema": "harn.consumer_composition_identity.v1",
  "pull": "9",
  "requiredPulls": ["7", "8"],
  "manifestSha256": "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"
}
```

`requiredPulls` contains the source's sorted adaptation numbers. The digest is
SHA-256 over `harn.consumer_composition_manifest.v1`, one NUL byte, and the exact
manifest bytes. The existing binding also records the tested consumer head's
hash and the exact Harn source, producer attempt, and archive manifest identity.

Public bindings and diagnostics exclude the private repository, branch, raw
consumer heads, and manifest body. The private source CLI's JSON stream must be
captured, never logged. Publication reauthenticates the manifest and components;
a manifest or a version 1 receipt can't authorize a composed source.
