import {
  mkdtempSync,
  mkdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, describe, expect, it } from "vitest";
import { load } from "js-yaml";
import { verifyOAuthClientDocument } from "./oauth-client-document.mjs";

const repositoryRoot = join(dirname(fileURLToPath(import.meta.url)), "..");
const scratch = [];
afterEach(() => {
  for (const directory of scratch.splice(0))
    rmSync(directory, { recursive: true });
});

function distribution(document) {
  const directory = mkdtempSync(join(tmpdir(), "harn-mcp-client-document-"));
  scratch.push(directory);
  if (document) {
    mkdirSync(join(directory, ".well-known"));
    writeFileSync(
      join(directory, ".well-known/oauth-client.json"),
      JSON.stringify(document),
    );
  }
  return directory;
}

describe("published MCP client identity", () => {
  it("runs the site build when the client identity owner changes", () => {
    const workflow = load(
      readFileSync(join(repositoryRoot, ".github/workflows/ci.yml"), "utf8"),
    );
    const filters = load(
      workflow.jobs.changes.steps.find((step) => step.id === "surface_filter")
        .with.filters,
    );
    expect(filters.docs_site).toContain("crates/harn-vm/src/mcp_auth.rs");
  });

  it("rejects the missing artifact produced by the old site", () => {
    expect(() =>
      verifyOAuthClientDocument(repositoryRoot, distribution()),
    ).toThrow("ENOENT");
  });

  it("rejects the old document without client_id and a mismatched identity", () => {
    const document = JSON.parse(
      readFileSync(
        join(repositoryRoot, "website/public/.well-known/oauth-client.json"),
        "utf8",
      ),
    );
    delete document.client_id;
    expect(() =>
      verifyOAuthClientDocument(repositoryRoot, distribution(document)),
    ).toThrow("must equal the client's identity URL");
    document.client_id = "https://different.example/client.json";
    expect(() =>
      verifyOAuthClientDocument(repositoryRoot, distribution(document)),
    ).toThrow("must equal the client's identity URL");
  });

  it("accepts the canonical document at the client's actual URL", () => {
    const document = JSON.parse(
      readFileSync(
        join(repositoryRoot, "website/public/.well-known/oauth-client.json"),
        "utf8",
      ),
    );
    expect(
      verifyOAuthClientDocument(repositoryRoot, distribution(document))
        .document,
    ).toEqual(document);
  });
});
