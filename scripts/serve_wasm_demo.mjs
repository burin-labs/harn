#!/usr/bin/env node

import { createReadStream, statSync } from "node:fs";
import { createServer } from "node:http";
import { extname, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const repoRoot = resolve(fileURLToPath(new URL("..", import.meta.url)));
const publicRoot = resolve(
  process.env.HARN_WASM_DEMO_ROOT ?? resolve(repoRoot, "crates/harn-wasm"),
);
const host = process.env.HARN_WASM_DEMO_HOST ?? "127.0.0.1";
const port = Number.parseInt(process.env.HARN_WASM_DEMO_PORT ?? "8765", 10);
const types = new Map([
  [".css", "text/css; charset=utf-8"],
  [".harn", "text/plain; charset=utf-8"],
  [".html", "text/html; charset=utf-8"],
  [".js", "text/javascript; charset=utf-8"],
  [".json", "application/json; charset=utf-8"],
  [".wasm", "application/wasm"],
]);

function resolveRequest(rawUrl) {
  const pathname = decodeURIComponent(new URL(rawUrl, "http://localhost").pathname);
  const relative = pathname === "/" ? "index.html" : pathname.replace(/^\/+/, "");
  const candidate = resolve(publicRoot, relative);
  if (candidate !== publicRoot && !candidate.startsWith(`${publicRoot}${sep}`)) return null;
  return candidate;
}

const server = createServer((request, response) => {
  let path;
  try {
    path = resolveRequest(request.url ?? "/");
  } catch {
    response.writeHead(400).end("invalid URL");
    return;
  }
  if (path === null) {
    response.writeHead(403).end("outside demo root");
    return;
  }
  try {
    if (!statSync(path).isFile()) throw new Error("not a file");
  } catch {
    response.writeHead(404).end("not found");
    return;
  }
  response.writeHead(200, {
    "Cache-Control": "no-store",
    "Content-Type": types.get(extname(path)) ?? "application/octet-stream",
    "Cross-Origin-Opener-Policy": "same-origin",
    "Cross-Origin-Resource-Policy": "same-origin",
  });
  createReadStream(path).pipe(response);
});

server.on("error", (error) => {
  process.stderr.write(
    `Unable to serve ${publicRoot} at http://${host}:${port}: ${error.message}\n`,
  );
  process.exitCode = 1;
});

server.listen(port, host, () => {
  process.stdout.write(
    `Portable Harn Kernel demo: http://${host}:${port} (root ${publicRoot})\n`,
  );
});
