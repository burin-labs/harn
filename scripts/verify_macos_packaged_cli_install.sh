#!/usr/bin/env bash
set -euo pipefail
# shellcheck source=scripts/lib/sha256.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/sha256.sh"

[[ "$(uname -s)" == Darwin ]] || { echo "This proof requires macOS." >&2; exit 2; }
source_sha="${PACKAGE_PROOF_SOURCE_SHA:?}"
[[ "$source_sha" =~ ^[0-9a-f]{40}$ ]] || exit 2
[[ "$(git rev-parse HEAD)" == "$source_sha" ]] || { echo "Source identity differs." >&2; exit 2; }
[[ -z "$(git status --porcelain --untracked-files=no)" ]] || { echo "Source is dirty." >&2; exit 2; }
output="${PACKAGE_PROOF_OUTPUT:?}"
mkdir "$output"
scratch="$(mktemp -d "${RUNNER_TEMP:?}/harn-packaged-install.XXXXXX")"
cleanup() {
  if [[ -d "$scratch" && ! -L "$scratch" && "$scratch" == "$RUNNER_TEMP"/harn-packaged-install.* ]]; then
    rm -rf -- "$scratch"
  fi
}
trap cleanup EXIT
package_sha=eb5aa010b0f2073414633b329efcc3bc10e46d81d5cc7fa7e2b2417c45cd446b
archive="$scratch/harn-cli-0.10.182.crate"
curl --fail --location --silent --show-error \
  https://static.crates.io/crates/harn-cli/harn-cli-0.10.182.crate --output "$archive"
[[ "$(sha256_file_hex "$archive")" == "$package_sha" ]] || { echo "Published fixture digest differs." >&2; exit 1; }
tar -xzf "$archive" -C "$scratch"
package="$scratch/harn-cli-0.10.182"
candidate=crates/harn-cli/src/tests/common/mod.rs
baseline="$package/src/tests/common/mod.rs"
cp "$baseline" "$output/published-common-mod.rs"
cp "$candidate" "$output/candidate-common-mod.rs"
# The published graph is immutable. Only the source gate under review changes.
sed 's/#\[cfg(all(test, target_os = "macos"))\]/#[cfg(target_os = "macos")]/' \
  "$candidate" > "$scratch/reversed-common-mod.rs"
cmp "$baseline" "$scratch/reversed-common-mod.rs"
[[ "$(grep -Fc '#[cfg(all(test, target_os = "macos"))]' "$candidate")" == 1 ]]
adapted_source_sha="$(sha256_file_hex "$candidate")"
printf '%s\n' "$adapted_source_sha" > "$output/source-adaptation.sha256"
before=0
cargo install --path "$package" --debug --locked --root "$scratch/installed" \
  --target-dir "$scratch/target" > "$output/before.log" 2>&1 || before=$?
[[ "$before" != 0 ]] || { echo "Baseline unexpectedly installed." >&2; exit 1; }
grep -E "couldn't read.*harn-vm/tests/support/sandbox_host.rs" "$output/before.log"
cp "$candidate" "$baseline"
cargo install --path "$package" --debug --locked --root "$scratch/installed" \
  --target-dir "$scratch/target" > "$output/after.log" 2>&1
"$scratch/installed/bin/harn" --version > "$output/version.txt"
"$scratch/installed/bin/harn" run -e \
  'fn main(harness: Harness) { harness.stdio.println("packaged-install-reached") }' \
  > "$output/smoke.txt"
grep -Fx packaged-install-reached "$output/smoke.txt"
PACKAGE_PROOF_CRATE_SHA="$package_sha" PACKAGE_PROOF_BASELINE_EXIT="$before" \
PACKAGE_PROOF_ADAPTED_SOURCE_SHA="$adapted_source_sha" python3 - "$output/receipt.json" <<'PY'
import json
import os
import pathlib
import sys

receipt = {
    "schema_version": "harn.macos_packaged_install.v1",
    "source_sha": os.environ["PACKAGE_PROOF_SOURCE_SHA"],
    "published_fixture_version": "0.10.182",
    "published_fixture_sha256": os.environ["PACKAGE_PROOF_CRATE_SHA"],
    "adaptation": "Only src/tests/common/mod.rs copied from the reviewed source; all dependencies use the published 0.10.182 locked graph.",
    "adapted_source_sha256": os.environ["PACKAGE_PROOF_ADAPTED_SOURCE_SHA"],
    "baseline_exit_code": int(os.environ["PACKAGE_PROOF_BASELINE_EXIT"]),
    "baseline_missing_sibling_include_observed": True,
    "adapted_install_exit_code": 0,
    "installed_script_smoke": "packaged-install-reached",
}
pathlib.Path(sys.argv[1]).write_text(json.dumps(receipt, indent=2) + "\n")
PY
cat "$output/receipt.json"
