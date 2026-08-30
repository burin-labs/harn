// Scan tracked text for tokens whose sha256 is on a committed denylist.
//
// The denylist stores hashes only, so this repository never carries the
// plaintext of the hostnames and addresses it refuses. Output names
// `path:line` and a hash prefix and NEVER the matched text — a public CI log
// echoing the match would leak exactly what the gate exists to remove.
//
// Tokens are split on [^A-Za-z0-9._-]+ and lowercased, so `host.local` and
// `host` are distinct tokens and each must be hashed separately.
//
// Usage: node scripts/scan_hashed_denylist.mjs <denylist-file>
//        node scripts/scan_hashed_denylist.mjs <denylist-file> --text-label <label>
//   default stdin: NUL-separated tracked paths (git ls-files -z)
//   --text-label stdin: text to scan; locations use the supplied public label
//   exit 0 = clean, 1 = hits (printed to stdout), 2 = usage/IO error

import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";

const denylistPath = process.argv[2];
if (!denylistPath) {
  process.stderr.write(
    "usage: scan_hashed_denylist.mjs <denylist-file> [--text-label <label>]\n",
  );
  process.exit(2);
}

let textLabel;
if (process.argv.length > 3) {
  if (
    process.argv[3] !== "--text-label" ||
    !/^[A-Za-z0-9._/-]+$/.test(process.argv[4] ?? "") ||
    process.argv.length !== 5
  ) {
    process.stderr.write(
      "usage: scan_hashed_denylist.mjs <denylist-file> [--text-label <label>]\n",
    );
    process.exit(2);
  }
  textLabel = process.argv[4];
}

const sha256 = (text) =>
  createHash("sha256").update(text, "utf8").digest("hex");

const banned = new Set(
  readFileSync(denylistPath, "utf8")
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => /^[0-9a-f]{64}$/.test(line)),
);
if (banned.size === 0) {
  process.stderr.write(`error: no hashes found in ${denylistPath}\n`);
  process.exit(2);
}

const tokenPattern = /[A-Za-z0-9._-]+/g;
// Cache token -> banned? across the whole tree; most tokens repeat heavily.
const verdict = new Map();
const hits = [];

const scanText = (text, location, skipBinary) => {
  if (skipBinary && text.includes(0)) return; // mirrors `git grep -I` for files
  const lines = text.toString("utf8").split("\n");
  for (let i = 0; i < lines.length; i += 1) {
    const matches = lines[i].match(tokenPattern);
    if (!matches) continue;
    for (const raw of matches) {
      const token = raw.toLowerCase();
      let bad = verdict.get(token);
      if (bad === undefined) {
        bad = banned.has(sha256(token));
        verdict.set(token, bad);
      }
      if (bad) {
        // Hash prefix only. The token itself never leaves this process.
        hits.push(`${location}:${i + 1}: sha256:${sha256(token).slice(0, 12)}`);
      }
    }
  }
};

if (textLabel !== undefined) {
  scanText(readFileSync(0), textLabel, false);
} else {
  const paths = readFileSync(0, "utf8").split("\0").filter(Boolean);
  for (const path of paths) {
    let text;
    try {
      text = readFileSync(path);
    } catch {
      continue; // deleted between ls-files and read
    }
    scanText(text, path, true);
  }
}

if (hits.length > 0) {
  process.stdout.write(hits.join("\n") + "\n");
  process.exit(1);
}
process.exit(0);
