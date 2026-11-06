const assert = require("node:assert/strict");
const Module = require("node:module");
const test = require("node:test");

test("the language client matches file-operation globs with the installed matcher", async () => {
  const originalLoad = Module._load;
  Module._load = function (request, ...args) {
    if (request === "vscode") {
      return {
        Uri: { parse: () => ({ path: "/workspace", with: ({ path }) => ({ fsPath: path }) }) },
        workspace: { onDidCreateFiles: () => ({ dispose() {} }) },
      };
    }
    return originalLoad.call(this, request, ...args);
  };
  let matchGlobPattern;
  let DidCreateFilesFeature;
  try {
    ({ matchGlobPattern } = require("vscode-languageclient/$test/common/utils/globPattern"));
    ({ DidCreateFilesFeature } = require("vscode-languageclient/$test/common/fileOperations"));
  } finally {
    Module._load = originalLoad;
  }
  assert.equal(matchGlobPattern("**/*.harn", { fsPath: "/workspace/lib/run.harn" }), true);
  assert.equal(matchGlobPattern("**/*.harn", { fsPath: "/workspace/lib/run.swift" }), false);
  assert.equal(matchGlobPattern({ baseUri: "file:///workspace", pattern: "**/*.harn" },
    { fsPath: "/workspace/lib/run.harn" }), true);
  assert.equal(matchGlobPattern({ baseUri: "file:///workspace", pattern: "**/*.harn" },
    { fsPath: "/other/lib/run.harn" }), false);
  const feature = new DidCreateFilesFeature({ code2ProtocolConverter: { asDidCreateFilesParams() {} } });
  feature.register({ id: "fixture", registerOptions: { filters: [{ pattern: { glob: "**/*.harn" } }] } });
  const matching = { fsPath: "/workspace/run.harn", scheme: "file" };
  const other = { fsPath: "/workspace/run.swift", scheme: "file" };
  assert.deepEqual((await feature.filter({ files: [matching, other] }, (item) => item)).files, [matching]);
  assert.throws(() => feature.register({ id: "empty", registerOptions: { filters: [{ pattern: { glob: "" } }] } }));
});
