#!/usr/bin/env python3
"""Refusing GitHub transport fixture for the real release-tag reader."""
import hashlib
import json
import os
from pathlib import Path
import sys

repository = "fixture/harn"
source = os.environ["PUBLICATION_SOURCE"]
case = os.environ.get("PUBLICATION_CASE", "valid")
targets = json.loads(os.environ["PUBLICATION_TARGETS"])
tag = "v1.2.3"
base = f"https://github.com/{repository}"
predicate = "https://harnlang.com/attestations/release-archive/v1"
publisher = {"id": 278545796, "login": "harn-release-bot[bot]", "type": "Bot"}
other_actor = {"id": 41898282, "login": "github-actions[bot]", "type": "Bot"}
with open(os.environ["PUBLICATION_TRACE"], "a", encoding="utf-8") as trace:
    trace.write(" ".join(sys.argv[1:3]) + "\n")


def refuse():
    print("unexpected or deliberately failed GitHub fixture request", file=sys.stderr)
    sys.exit(71)


def encoded(value):
    return json.dumps(value, sort_keys=True).encode()


index = {"tag": tag, "version": "1.2.3", "release_url": f"{base}/releases/tag/{tag}", "assets": {}}
archives = []
assets = []
for ordinal, target in enumerate(targets, 1):
    name = f"harn-{target}" + (".zip" if target == "x86_64-pc-windows-msvc" else ".tar.gz")
    digest = hashlib.sha256(target.encode()).hexdigest()
    index["assets"][target] = {"filename": name, "sha256": digest, "size": 100 + ordinal,
                               "url": f"{base}/releases/download/{tag}/{name}"}
    archives.append({"kind": "archive", "target": target, "artifact": f"harn-{target}",
                     "file": name, "sha256": digest, "attestationPredicateType": predicate})
    assets.append({"id": ordinal, "name": name, "size": 100 + ordinal, "state": "uploaded", "uploader": publisher,
                   "digest": f"sha256:{digest}"})
if case == "incomplete-index":
    del index["assets"][targets[0]]
index_bytes = encoded(index)
index_digest = hashlib.sha256(index_bytes).hexdigest()
manifest = {"schemaVersion": "burin-labs.candidate_manifest.v1", "repository": repository,
            "sourceCommit": source, "runId": "42", "runAttempt": "1", "artifacts": archives + [
                {"kind": "asset-index", "file": "release-assets.json", "sha256": index_digest,
                 "attestationPredicateType": predicate}]}
if case == "wrong-manifest-source":
    manifest["sourceCommit"] = "f" * 40
manifest_bytes = encoded(manifest)
for ordinal, (name, data) in enumerate([
    ("candidate-manifest.json", manifest_bytes), ("release-assets.json", index_bytes)
], 100):
    assets.append({"id": ordinal, "name": name, "size": len(data), "state": "uploaded", "uploader": publisher,
                   "digest": "sha256:" + hashlib.sha256(data).hexdigest()})
if case == "incomplete-assets":
    assets.pop(0)
if case == "wrong-archive-digest":
    assets[0]["digest"] = "sha256:" + "0" * 64
if case == "wrong-manifest-bytes":
    assets[-2]["digest"] = "sha256:" + "0" * 64
if case == "nonpublisher-manifest":
    assets[-2]["uploader"] = other_actor
if case == "nonpublisher-index":
    assets[-1]["uploader"] = other_actor
if case == "nonpublisher-archive":
    assets[0]["uploader"] = other_actor
# SDK publication legitimately adds assets under a different GitHub actor.
assets.append({"id": 200, "name": "harn-sdk-python.tar.gz", "size": 100, "state": "uploaded",
               "digest": "sha256:" + "0" * 64, "uploader": other_actor})
release = {"id": 1, "tag_name": tag, "draft": False, "prerelease": False,
           "published_at": "2026-10-09T00:00:00Z", "html_url": f"{base}/releases/tag/{tag}",
           "author": other_actor if case == "nonpublisher-author" else publisher, "assets": assets}
if case == "draft-release":
    release["draft"] = True

args = sys.argv[1:]
if args == ["api", f"repos/{repository}/git/commits/{source}"]:
    if case == "http-failure":
        refuse()
    print(json.dumps({"sha": source, "verification": {"verified": case != "unsigned-source", "reason": "valid"}}))
elif args == ["api", f"repos/{repository}/releases/tags/{tag}"]:
    if case == "unpublished-source":
        refuse()
    print(json.dumps(release))
elif args[:-1] == ["release", "download", tag, "--repo", repository, "--pattern",
                  "candidate-manifest.json", "--pattern", "release-assets.json", "--dir"]:
    Path(args[-1], "candidate-manifest.json").write_bytes(manifest_bytes)
    Path(args[-1], "release-assets.json").write_bytes(index_bytes)
elif len(args) == 15 and args[:2] == ["attestation", "verify"] and args[3:] == [
    "--repo", repository, "--signer-workflow", f"{repository}/.github/workflows/build-release-binaries.yml",
    "--source-digest", source, "--source-ref", "refs/heads/main", "--predicate-type", predicate, "--format", "json"
]:
    if Path(args[2]).name != "release-assets.json" or Path(args[2]).read_bytes() != index_bytes:
        refuse()
    if case == "attestation-failure":
        refuse()
    certificate = {"issuer": "https://token.actions.githubusercontent.com", "sourceRepositoryURI": base,
                   "sourceRepositoryDigest": source, "sourceRepositoryRef": "refs/heads/main",
                   "buildSignerURI": f"{base}/.github/workflows/build-release-binaries.yml@refs/heads/main",
                   "buildSignerDigest": source, "buildTrigger": "workflow_dispatch",
                   "runInvocationURI": f"{base}/actions/runs/42/attempts/1"}
    mutations = {"wrong-workflow": ("buildSignerURI", f"{base}/.github/workflows/other.yml@refs/heads/main"),
                 "wrong-ref": ("sourceRepositoryRef", "refs/heads/untrusted"),
                 "wrong-source": ("sourceRepositoryDigest", "f" * 40),
                 "wrong-run": ("runInvocationURI", f"{base}/actions/runs/43/attempts/1"),
                 "wrong-attempt": ("runInvocationURI", f"{base}/actions/runs/42/attempts/2")}
    if case in mutations:
        key, value = mutations[case]
        certificate[key] = value
    statement = {"predicateType": predicate,
                 "subject": [{"name": "release-assets.json", "digest": {"sha256": index_digest}}],
                 "predicate": {"schemaVersion": "harn.release_files_provenance.v1", "phase": "candidate",
                               "repository": repository, "sourceCommit": source, "version": "1.2.3",
                               "workflow": {"runId": "42", "runAttempt": "1"}}}
    if case == "wrong-subject":
        statement["subject"][0]["digest"]["sha256"] = "0" * 64
    if case == "wrong-predicate":
        statement["predicate"]["schemaVersion"] = "untrusted.v1"
    result = [{"verificationResult": {"signature": {"certificate": certificate},
               "verifiedTimestamps": [{}], "statement": statement}}]
    print(json.dumps([] if case == "empty-attestation" else result))
else:
    refuse()
