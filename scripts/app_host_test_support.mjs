import { readFile } from "node:fs/promises";
import vm from "node:vm";

const repository = new URL("../", import.meta.url);

export function plain(value) {
  return JSON.parse(JSON.stringify(value));
}

export async function runSource(context, relativePath, expose = "") {
  const source = await readFile(new URL(relativePath, repository), "utf8");
  vm.runInContext(`${source}\n${expose}`, context, { filename: relativePath });
}

export async function installProtocol(values = {}) {
  const context = vm.createContext(values);
  await runSource(
    context,
    "crates/harn-cli/src/commands/app_host/protocol.js",
    "globalThis.protocol = HarnAppHostProtocol;",
  );
  return context;
}
