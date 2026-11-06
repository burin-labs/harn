// The language client only uses makeRe() as an empty-pattern check. Minimatch
// 11 removed that method; its parsed set supplies the same check. Keep this
// patch pinned to the measured upstream files until the client supports v11.
const { createHash } = require("node:crypto");
const { readFileSync, writeFileSync } = require("node:fs");
const { join } = require("node:path");

const root = join(__dirname, "..", "node_modules", "vscode-languageclient");
const version = JSON.parse(readFileSync(join(root, "package.json"), "utf8")).version;
if (version !== "10.1.2") throw new Error(`Review the matcher patch for vscode-languageclient ${version}`);
const files = [
  ["lib/common/utils/globPattern.js", "ec8f8ed49366dc71c63a6908f0de06568086058cef3e59d6665df4c721826129", "6bab870a3edd1d991af511e2e808afbaac10e236cb36422716b551403db75b7b"],
  ["lib/common/fileOperations.js", "27021d92f7e2bb0094f0044d9516bfb3d76427e350b9a0945dac00e294f88f01", "46f1ec30d94ec7f7658ff219d4f00f973c9629f648ebbf81e311022f8e7ff02d"],
];
const digest = (text) => createHash("sha256").update(text).digest("hex");
const replacements = files.map(([relative, original, patched]) => {
  const path = join(root, relative);
  const source = readFileSync(path, "utf8");
  const actual = digest(source);
  if (actual === patched) return null;
  if (actual !== original) throw new Error(`Unrecognized language-client source: ${relative}`);
  const replacement = source.replace("!matcher.makeRe()", "matcher.set.length === 0");
  if (digest(replacement) !== patched) throw new Error(`Language-client patch mismatch: ${relative}`);
  return { path, replacement };
});
for (const item of replacements) if (item) writeFileSync(item.path, item.replacement);
