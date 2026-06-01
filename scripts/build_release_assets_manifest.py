#!/usr/bin/env python3
"""Emit `release-assets.json` for a Harn GitHub release.

Downstream consumers (`burin-code/scripts/fetch-harn.sh`, the Scoop
manifest generator, the `@burin/cli` npm postinstall) read this file
to verify and fetch the correct per-architecture binary archive
without scraping the GitHub releases API.

Schema:
    {
      "version": "0.8.21",
      "tag": "v0.8.21",
      "release_url": "https://github.com/.../releases/tag/v0.8.21",
      "assets": {
        "<rust-target-triple>": {
          "filename": "harn-<triple>.tar.gz" | "harn-<triple>.zip",
          "url":      "https://github.com/.../releases/download/<tag>/<filename>",
          "sha256":   "<hex>",
          "size":     <bytes>,
          "format":   "tar.gz" | "zip",
          "binaries": ["harn", "harn-dap", "harn-lsp"] | ["harn.exe", ...]
        }
      }
    }

The schema is keyed by Rust target triple so consumers can map their
own platform detection (e.g. `uname -s`/`uname -m`, Node `process.platform`/
`process.arch`) onto a single lookup. See
`docs/src/dev/release-assets-manifest.md` for the consumer contract.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import sys
from pathlib import Path

REPO = "burin-labs/harn"

# Rust target triple → packaging metadata. Kept in lockstep with the
# release-binary build matrix in
# `.github/workflows/build-release-binaries.yml`. Adding a target here
# without adding it to the matrix produces a missing-file error at
# manifest time, which is the desired fail-loud behavior.
TARGETS: dict[str, dict[str, object]] = {
    "aarch64-apple-darwin": {
        "filename": "harn-aarch64-apple-darwin.tar.gz",
        "format": "tar.gz",
        "binaries": ["harn", "harn-dap", "harn-lsp"],
    },
    "x86_64-apple-darwin": {
        "filename": "harn-x86_64-apple-darwin.tar.gz",
        "format": "tar.gz",
        "binaries": ["harn", "harn-dap", "harn-lsp"],
    },
    "x86_64-unknown-linux-gnu": {
        "filename": "harn-x86_64-unknown-linux-gnu.tar.gz",
        "format": "tar.gz",
        "binaries": ["harn", "harn-dap", "harn-lsp", "harn-container-probe"],
    },
    "aarch64-unknown-linux-gnu": {
        "filename": "harn-aarch64-unknown-linux-gnu.tar.gz",
        "format": "tar.gz",
        "binaries": ["harn", "harn-dap", "harn-lsp", "harn-container-probe"],
    },
    "x86_64-pc-windows-msvc": {
        "filename": "harn-x86_64-pc-windows-msvc.zip",
        "format": "zip",
        "binaries": ["harn.exe", "harn-dap.exe", "harn-lsp.exe"],
    },
}


def sha256_of(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(64 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()


def build_manifest(artifacts_dir: Path, tag: str) -> dict[str, object]:
    version = tag.lstrip("v")
    download_base = f"https://github.com/{REPO}/releases/download/{tag}"
    release_url = f"https://github.com/{REPO}/releases/tag/{tag}"

    assets: dict[str, dict[str, object]] = {}
    missing: list[str] = []
    for triple, meta in TARGETS.items():
        filename = str(meta["filename"])
        path = artifacts_dir / filename
        if not path.is_file():
            missing.append(filename)
            continue
        assets[triple] = {
            "filename": filename,
            "url": f"{download_base}/{filename}",
            "sha256": sha256_of(path),
            "size": path.stat().st_size,
            "format": meta["format"],
            "binaries": meta["binaries"],
        }

    if missing:
        raise SystemExit(
            "release-assets manifest is missing required archives: "
            + ", ".join(sorted(missing))
        )

    return {
        "version": version,
        "tag": tag,
        "release_url": release_url,
        "assets": assets,
    }


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--artifacts-dir", required=True, type=Path,
                   help="Directory holding the downloaded per-target archives.")
    p.add_argument("--tag", required=True,
                   help='Release tag, e.g. "v0.8.21".')
    p.add_argument("--output", required=True, type=Path,
                   help="Where to write release-assets.json.")
    return p.parse_args()


def main() -> int:
    args = parse_args()
    manifest = build_manifest(args.artifacts_dir, args.tag)
    args.output.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(f"Wrote {args.output} ({len(manifest['assets'])} targets)", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
