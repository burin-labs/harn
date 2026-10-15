# Release a source package

Use this procedure to build and publish a Smart Ship source archive from Harn's
main branch. Building a candidate and publishing it are separate actions.
Registry adoption is a later step.

## Before you start

Require a published Harn 0.10.166 runtime. A green runtime candidate alone is
not publication. The workflows install that release through the existing
checksum-checked setup action. They do not build a native runtime.

Choose a reviewed source on main that contains the intended package version
and all of its release acceptance evidence. For Smart Ship, that includes the
GitHub controller acceptance and a proved runtime floor. The package delivery
tools alone do not prove those product claims.

The repository needs Actions artifact and attestation support. Publishing uses
the existing `RELEASE_APP_CLIENT_ID` and `RELEASE_APP_PRIVATE_KEY` secrets of
the shared release workflow. The candidate builder needs no publication key.
Do not create another release credential for this procedure.

## Build the candidate

1. In a clean Harn checkout, read the intended main commit with
   `git rev-parse origin/main`. Record its full SHA.
2. Open **Actions → Build Smart Ship package → Run workflow**. Select main and
   enter that SHA as `expected_source_sha`. If main moved before the run
   started, the source check refuses the build. Choose and review the new
   source before trying again.
3. Wait for that exact run to finish successfully. Inspect its nonempty test
   inventory and complete verification report. Pending, failed or absent
   evidence is not a release candidate.
4. Record the run ID, source SHA and attempt. Keep the run artifacts until
   promotion finishes. They expire after 30 days.

The run packs the package once, unpacks and tests those bytes, and retains the
archive, checksums, metadata, full reports and release notes. It attests these
files and uploads a source-bound candidate manifest. It also uploads a separate
single-file copy of the metadata for the bounded promotion reader.

## Promote the tested files

1. Open **Actions → Promote Smart Ship package → Run workflow** on main.
2. Enter the recorded run ID as `candidate_run_id` and full source SHA as
   `candidate_source_sha`. There is no version input.
3. Wait for both the selection job and shared promotion job to succeed.
   Selection reads the exact producer, attempt, manifest and metadata. The
   shared publisher checks the files and their attestations again.
4. Confirm the release tag is `harn-smart-ship/v<package version>`, points to
   the recorded source, and contains the candidate's archive and evidence.
   Confirm the native runtime's latest release did not change. Native crate and
   SDK publication accept runtime tags only and must not append package assets.

Publication releases the tested bytes without rebuilding them. It does not
advance a Smart Ship checkpoint or transfer repository authority. Install the
release from its exact archive and checksum through the package manager before
recording downstream adoption. An archive release, a registry entry and a
verified consumer are separate proofs.

## Recover a refusal

For failed tests or changed inputs, inspect the preserved diagnostics, repair
the source and build a new candidate. A failed run emits no manifest eligible
for promotion. Do not substitute a report from another run.

For a changed attempt, let the current attempt finish and inspect its evidence
before selecting it again. For expired artifacts, build a new candidate. For a
tag conflict or byte mismatch, retain the refusal and inspect the existing
release. Never replace published files or move a tag to make the check pass.
