const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { spawn, spawnSync } = require("node:child_process");
const test = require("node:test");
const { pathToFileURL } = require("node:url");

class LspClient {
  constructor(server) {
    this.server = server;
    this.nextId = 1;
    this.pending = new Map();
    this.diagnostics = new Map();
    this.diagnosticWaiters = new Map();
    this.buffer = Buffer.alloc(0);
    this.stderr = "";
    this.exitError = undefined;

    server.stdout.on("data", (chunk) => this.read(chunk));
    server.stderr.on("data", (chunk) => {
      this.stderr += chunk.toString("utf8");
    });
    server.on("error", (err) => {
      this.exitError = err;
      this.failPending(err);
    });
    server.on("exit", (code, signal) => {
      const error = new Error(
        `harn-lsp exited code=${code} signal=${signal}: ${this.stderr}`,
      );
      this.exitError = error;
      this.failPending(error);
    });
  }

  // Bound an unresponsive server; this is not a performance assertion.
  static REQUEST_TIMEOUT_MS = 15_000;

  failPending(error) {
    for (const { reject, timeout } of [
      ...this.pending.values(),
      ...this.diagnosticWaiters.values(),
    ]) {
      clearTimeout(timeout);
      reject(error);
    }
    this.pending.clear();
    this.diagnosticWaiters.clear();
  }

  waitForDiagnostics(uri) {
    if (this.exitError) return Promise.reject(this.exitError);
    if (this.diagnostics.has(uri))
      return Promise.resolve(this.diagnostics.get(uri));
    return new Promise((resolve, reject) => {
      const timeout = setTimeout(() => {
        this.diagnosticWaiters.delete(uri);
        reject(
          new Error(
            `background analysis did not publish diagnostics for ${uri}`,
          ),
        );
      }, LspClient.REQUEST_TIMEOUT_MS);
      this.diagnosticWaiters.set(uri, { resolve, reject, timeout });
    });
  }

  request(method, params, { timeoutMs = LspClient.REQUEST_TIMEOUT_MS } = {}) {
    if (this.exitError) {
      return Promise.reject(this.exitError);
    }
    const id = this.nextId++;
    this.write({ jsonrpc: "2.0", id, method, params });
    return new Promise((resolve, reject) => {
      const timeout =
        timeoutMs === null
          ? undefined
          : setTimeout(() => {
              this.pending.delete(id);
              reject(
                new Error(`timed out waiting for ${method}: ${this.stderr}`),
              );
            }, timeoutMs);
      this.pending.set(id, { resolve, reject, timeout });
    });
  }

  notify(method, params) {
    this.write({ jsonrpc: "2.0", method, params });
  }

  write(message) {
    const body = Buffer.from(JSON.stringify(message), "utf8");
    this.server.stdin.write(`Content-Length: ${body.length}\r\n\r\n`, "ascii");
    this.server.stdin.write(body);
  }

  read(chunk) {
    this.buffer = Buffer.concat([this.buffer, chunk]);
    while (true) {
      const headerEnd = this.buffer.indexOf("\r\n\r\n");
      if (headerEnd === -1) {
        return;
      }
      const header = this.buffer.slice(0, headerEnd).toString("ascii");
      const match = /Content-Length: (\d+)/i.exec(header);
      assert.ok(match, `missing Content-Length in ${header}`);
      const length = Number(match[1]);
      const messageStart = headerEnd + 4;
      const messageEnd = messageStart + length;
      if (this.buffer.length < messageEnd) {
        return;
      }
      const message = JSON.parse(
        this.buffer.slice(messageStart, messageEnd).toString("utf8"),
      );
      this.buffer = this.buffer.slice(messageEnd);
      if (message.method === "textDocument/publishDiagnostics") {
        const { uri } = message.params;
        this.diagnostics.set(uri, message.params);
        const waiter = this.diagnosticWaiters.get(uri);
        if (waiter) {
          clearTimeout(waiter.timeout);
          this.diagnosticWaiters.delete(uri);
          waiter.resolve(message.params);
        }
      }
      if (Object.prototype.hasOwnProperty.call(message, "id")) {
        const pending = this.pending.get(message.id);
        if (!pending) {
          continue;
        }
        this.pending.delete(message.id);
        clearTimeout(pending.timeout);
        if (message.error) {
          pending.reject(new Error(JSON.stringify(message.error)));
        } else {
          pending.resolve(message.result);
        }
      }
    }
  }

  async stop() {
    if (!this.server.killed) {
      try {
        await this.request("shutdown", null);
        this.notify("exit", {});
      } catch {
        this.server.kill();
      }
    }
  }
}

function repoRoot() {
  return path.resolve(__dirname, "../../..");
}

function fileUri(filePath) {
  return pathToFileURL(filePath).toString();
}

function lspEnvironment() {
  const env = { ...process.env, HARN_LLM_CALLS_DISABLED: "1" };
  delete env.HARN_LSP_BIN;
  return env;
}

function resolveLspBinary(root) {
  const suppliedBinary = process.env.HARN_LSP_BIN || process.env.HARN_BIN;
  if (suppliedBinary) {
    assert.ok(
      fs.existsSync(suppliedBinary),
      `LSP binary does not exist: ${suppliedBinary}`,
    );
    return suppliedBinary;
  }

  assert.notEqual(
    process.env.CI,
    "true",
    "CI must supply the verified Harn artifact through HARN_LSP_BIN or HARN_BIN",
  );

  const resolver = path.join(root, "scripts/harn_bin.sh");
  const env = lspEnvironment();
  const options = { cwd: root, env, encoding: "utf8" };
  const resolved = spawnSync("bash", [resolver, "--print"], options);
  assert.equal(
    resolved.status,
    0,
    `Harn binary resolution failed\nstdout:\n${resolved.stdout}\nstderr:\n${resolved.stderr}`,
  );
  const binary = resolved.stdout.trim();
  assert.ok(
    binary && fs.existsSync(binary),
    `Harn binary does not exist: ${binary}`,
  );
  return binary;
}

function positionOf(source, needle) {
  const offset = source.indexOf(needle);
  assert.notEqual(offset, -1, `missing ${needle}`);
  const before = source.slice(0, offset);
  const lines = before.split("\n");
  return {
    line: lines.length - 1,
    character: lines[lines.length - 1].length,
  };
}

test("VS Code-facing LSP surface supports on-type formatting, folding, and call hierarchy", async (t) => {
  const root = repoRoot();
  const lspBinary = resolveLspBinary(root);
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "harn-lsp-"));
  const server = spawn(lspBinary, [], {
    argv0: "harn-lsp",
    cwd: root,
    env: lspEnvironment(),
    stdio: ["pipe", "pipe", "pipe"],
  });
  const client = new LspClient(server);
  t.after(async () => {
    await client.stop();
    fs.rmSync(dir, { recursive: true, force: true });
  });

  const initialize = await client.request("initialize", {
    processId: process.pid,
    rootUri: fileUri(dir),
    capabilities: {},
  });
  const capabilities = initialize.capabilities;
  assert.deepEqual(capabilities.documentOnTypeFormattingProvider, {
    firstTriggerCharacter: ";",
    moreTriggerCharacter: ["}"],
  });
  assert.equal(capabilities.foldingRangeProvider, true);
  assert.equal(capabilities.callHierarchyProvider, true);
  client.notify("initialized", {});

  const source = [
    "pub fn callee(value){",
    "return value;",
    "}",
    "",
    "fn helper(value) {",
    "  return callee(value)",
    "}",
    "",
    "pipeline main(task) {",
    '  let prompt = """',
    "    first",
    "    second",
    '  """',
    "  match task {",
    '    "one" -> {',
    "      helper(task)",
    "      callee(task)",
    "    }",
    "    _ -> { callee(task) }",
    "  }",
    "}",
    "",
  ].join("\n");

  const file = path.join(dir, "surface.harn");
  fs.writeFileSync(file, source);
  const sibling = path.join(dir, "caller.harn");
  const siblingSource =
    'import { callee } from "./surface"\nfn external(value) { return callee(value) }\n';
  fs.writeFileSync(sibling, siblingSource);
  const decoy = path.join(dir, "decoy.harn");
  const decoySource =
    "fn callee(value) { return value }\nfn unrelated(value) { return callee(value) }\n";
  fs.writeFileSync(decoy, decoySource);
  const uri = fileUri(file);

  client.notify("textDocument/didOpen", {
    textDocument: {
      uri,
      languageId: "harn",
      version: 1,
      text: source,
    },
  });
  for (const [filePath, text] of [
    [sibling, siblingSource],
    [decoy, decoySource],
  ]) {
    client.notify("textDocument/didOpen", {
      textDocument: {
        uri: fileUri(filePath),
        languageId: "harn",
        version: 1,
        text,
      },
    });
  }

  // Navigation must survive completed background analysis, including a
  // dependency-triggered retry. An immediate request only sees provisional
  // syntax and can pass while the later analysis erases it.
  await Promise.all(
    [uri, fileUri(sibling), fileUri(decoy)].map((opened) =>
      client.waitForDiagnostics(opened),
    ),
  );

  const edits = await client.request("textDocument/onTypeFormatting", {
    textDocument: { uri },
    position: positionOf(source, "return value;"),
    ch: ";",
    options: { tabSize: 2, insertSpaces: true },
  });
  assert.ok(edits.length >= 1, "expected on-type formatting edit");
  assert.match(edits[0].newText, /fn callee\(value\) \{/);
  assert.match(edits[0].newText, /return value/);

  const folds = await client.request("textDocument/foldingRange", {
    textDocument: { uri },
  });
  assert.ok(
    folds.some((range) => range.startLine === 8 && range.endLine === 20),
    `expected main pipeline fold: ${JSON.stringify(folds)}`,
  );
  assert.ok(
    folds.some((range) => range.startLine === 9 && range.endLine === 12),
    `expected multiline string fold: ${JSON.stringify(folds)}`,
  );
  assert.ok(
    folds.some((range) => range.startLine === 14 && range.endLine === 16),
    `expected multiline match arm fold: ${JSON.stringify(folds)}`,
  );

  const prepared = await client.request("textDocument/prepareCallHierarchy", {
    textDocument: { uri },
    position: positionOf(source, "callee(value){"),
  });
  assert.ok(
    prepared?.length,
    "callee must remain available after background analysis settles",
  );
  assert.equal(prepared[0].name, "callee");

  const incoming = await client.request("callHierarchy/incomingCalls", {
    item: prepared[0],
  });
  assert.ok(
    incoming.some((call) => call.from.name === "helper"),
    `expected helper incoming call: ${JSON.stringify(incoming)}`,
  );
  assert.ok(
    incoming.some((call) => call.from.name === "main"),
    `expected main incoming call: ${JSON.stringify(incoming)}`,
  );
  assert.ok(
    incoming.some(
      (call) =>
        call.from.name === "external" && call.from.uri === fileUri(sibling),
    ),
    `expected a caller from the imported workspace module: ${JSON.stringify(incoming)}`,
  );
  assert.ok(
    !incoming.some((call) => call.from.name === "unrelated"),
    `a same-named function in another module is not this callee: ${JSON.stringify(incoming)}`,
  );

  const main = await client.request("textDocument/prepareCallHierarchy", {
    textDocument: { uri },
    position: positionOf(source, "main(task)"),
  });
  assert.equal(
    main?.[0]?.name,
    "main",
    "main must remain available after background analysis settles",
  );
  const outgoing = await client.request("callHierarchy/outgoingCalls", {
    item: main[0],
  });
  assert.ok(
    outgoing.some((call) => call.to.name === "helper"),
    `expected helper outgoing call: ${JSON.stringify(outgoing)}`,
  );
  assert.ok(
    outgoing.some((call) => call.to.name === "callee"),
    `expected callee outgoing call: ${JSON.stringify(outgoing)}`,
  );
});
