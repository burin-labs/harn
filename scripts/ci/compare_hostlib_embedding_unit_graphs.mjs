import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";

function canonical(value) {
  if (Array.isArray(value)) return value.map(canonical);
  if (value !== null && typeof value === "object") {
    return Object.fromEntries(
      Object.keys(value)
        .sort()
        .map((key) => [key, canonical(value[key])]),
    );
  }
  return value;
}

function graph(path) {
  const data = JSON.parse(readFileSync(path, "utf8"));
  assert.ok(
    Array.isArray(data.units) && data.units.length > 0,
    "Cargo unit census is empty",
  );
  assert.ok(
    Array.isArray(data.roots) && data.roots.length > 0,
    "Cargo root census is empty",
  );
  const memo = new Map();
  const visiting = new Set();
  function fingerprint(index) {
    assert.ok(
      Number.isInteger(index) && index >= 0 && index < data.units.length,
      "Invalid Cargo unit index",
    );
    if (memo.has(index)) return memo.get(index);
    assert.ok(!visiting.has(index), "Cargo unit graph has a cycle");
    visiting.add(index);
    const { dependencies, ...unit } = data.units[index];
    assert.ok(Array.isArray(dependencies), "Cargo dependency census is absent");
    const edges = dependencies
      .map(({ index: child, ...edge }) =>
        JSON.stringify(canonical({ ...edge, unit: fingerprint(child) })),
      )
      .sort();
    const value = createHash("sha256")
      .update(JSON.stringify(canonical({ unit, edges })))
      .digest("hex");
    visiting.delete(index);
    memo.set(index, value);
    return value;
  }
  return {
    ...data,
    fingerprints: new Map(data.units.map((unit, i) => [fingerprint(i), unit])),
  };
}

const [selection, writerPath, buildPath, testPath] = process.argv.slice(2);
assert.ok(
  selection && writerPath && buildPath && testPath,
  "Expected workspace selection, writer, build and test graphs",
);
const writer = graph(writerPath);
const embeddingPackages = new Set(
  writer.units
    .filter((unit) => unit.target.name === "harn_hostlib")
    .map((unit) => unit.pkg_id),
);
assert.equal(
  embeddingPackages.size,
  1,
  "Workspace embedding package is ambiguous",
);
for (const [operation, path] of [
  ["build", buildPath],
  ["test", testPath],
]) {
  const candidate = graph(path);
  assert.equal(
    candidate.roots.length,
    1,
    "Embedding must select only its example",
  );
  const root = candidate.units[candidate.roots[0]];
  assert.ok(
    embeddingPackages.has(root.pkg_id),
    "Example must belong to the workspace hostlib package",
  );
  assert.equal(root.target.name, "foreground_command");
  assert.deepEqual(root.target.kind, ["example"]);
  assert.equal(
    root.mode,
    operation,
    "Ordinary and test executables must both compile",
  );
  assert.ok(
    candidate.units.every((unit) => unit.target.name !== "harn_cli"),
    "Standalone embedding must not link the CLI",
  );
  const dependencies = [...candidate.fingerprints].filter(
    ([, unit]) => unit.target.name !== "foreground_command",
  );
  assert.ok(dependencies.length > 0, "Embedding dependency census is empty");
  const missing = dependencies.filter(
    ([identity]) => !writer.fingerprints.has(identity),
  );
  console.log(
    JSON.stringify({
      selection,
      operation,
      dependencies: dependencies.length,
      missing_from_workspace: missing.length,
      example_roots: candidate.roots.length,
      linked_cli: false,
    }),
  );
  if (missing.length > 0) {
    console.error(
      `Embedding ${operation} would recompile dependencies after the ${selection} workspace build:`,
    );
    console.error(
      missing
        .slice(0, 3)
        .map(([, unit]) => `${unit.pkg_id} ${unit.target.name}`)
        .join("\n"),
    );
    process.exitCode = 1;
  }
}
