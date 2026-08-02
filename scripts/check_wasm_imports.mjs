#!/usr/bin/env node

import { readFileSync } from "node:fs";

const wasmPath = process.argv[2];
if (!wasmPath) {
  console.error("usage: node scripts/check_wasm_imports.mjs <core-wasm-module>");
  process.exit(2);
}

const bytes = readFileSync(wasmPath);
const wasmModule = new WebAssembly.Module(bytes);
const imports = WebAssembly.Module.imports(wasmModule);
const exports = WebAssembly.Module.exports(wasmModule);

// wasm-bindgen's browser adapter needs only exception construction/throwing and
// externref-table initialization from its generated sibling JavaScript module.
// Keep this an allowlist: a newly introduced clock, random, network, file,
// process, model, memory, table, or other host import must fail review loudly.
const allowedFunctionNames = [
  /^__wbg___wbindgen_throw_[0-9a-f]+$/u,
  /^__wbg_Error_[0-9a-f]+$/u,
  /^__wbindgen_init_externref_table$/u,
];

const denied = imports.filter((entry) =>
  entry.module !== "./harn_wasm_bg.js"
  || entry.kind !== "function"
  || !allowedFunctionNames.some((pattern) => pattern.test(entry.name))
);

const exportNames = new Set(exports.map((entry) => entry.name));
const missingKernelExports = ["compile", "start", "resume"].filter(
  (name) => !exportNames.has(name),
);

const receipt = {
  schemaVersion: 1,
  module: wasmPath,
  imports,
  exports,
  ambientAuthorityImports: denied,
  missingKernelExports,
};

console.log(JSON.stringify(receipt, null, 2));
if (denied.length > 0 || missingKernelExports.length > 0) {
  console.error(
    "portable browser Wasm violated its reviewed import/export contract",
  );
  process.exit(1);
}
