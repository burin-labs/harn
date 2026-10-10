import { readFileSync } from "node:fs";
import { join } from "node:path";

// The MCP client's constant owns its identity URL. Validate the published
// artifact against that owner, so a site rewrite cannot silently remove it.
export function verifyOAuthClientDocument(repositoryRoot, distributionRoot) {
  const source = readFileSync(
    join(repositoryRoot, "crates/harn-vm/src/mcp_auth.rs"),
    "utf8",
  );
  const matches = [
    ...source.matchAll(
      /pub const DEFAULT_MCP_OAUTH_CLIENT_ID_METADATA_DOCUMENT_URL: &str =\s*"([^"]+)";/g,
    ),
  ];
  if (matches.length !== 1) {
    throw new Error(
      "MCP client identity URL was not measured from its Rust owner",
    );
  }
  const clientId = matches[0][1];
  const url = new URL(clientId);
  if (
    url.protocol !== "https:" ||
    url.search ||
    url.hash ||
    !url.pathname.endsWith(".json")
  ) {
    throw new Error("MCP client identity must name an HTTPS JSON document");
  }
  const document = JSON.parse(
    readFileSync(join(distributionRoot, url.pathname), "utf8"),
  );
  if (document.client_id !== clientId) {
    throw new Error(
      "Published MCP client_id must equal the client's identity URL",
    );
  }
  if (
    document.token_endpoint_auth_method !== "none" ||
    document.application_type !== "native"
  ) {
    throw new Error(
      "Published MCP client identity must remain a public native client",
    );
  }
  if (
    !Array.isArray(document.redirect_uris) ||
    document.redirect_uris.length === 0
  ) {
    throw new Error(
      "Published MCP client identity must declare its callback URLs",
    );
  }
  return { clientId, document };
}
