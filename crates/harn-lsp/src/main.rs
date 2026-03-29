mod constants;
mod document;
mod handlers;
mod helpers;
mod references;
mod semantic_tokens;
mod symbols;

use std::collections::HashMap;
use std::sync::Mutex;

use document::DocumentState;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LspService, Server};

// ---------------------------------------------------------------------------
// LSP backend
// ---------------------------------------------------------------------------

struct HarnLsp {
    client: Client,
    documents: Mutex<HashMap<Url, DocumentState>>,
}

impl HarnLsp {
    fn new(client: Client) -> Self {
        Self {
            client,
            documents: Mutex::new(HashMap::new()),
        }
    }
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(HarnLsp::new);

    Server::new(stdin, stdout, socket).serve(service).await;
}
