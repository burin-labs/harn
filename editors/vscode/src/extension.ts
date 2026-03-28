import * as vscode from "vscode";
import {
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
} from "vscode-languageclient/node";

let client: LanguageClient | undefined;

export function activate(context: vscode.ExtensionContext) {
  const config = vscode.workspace.getConfiguration("harn");
  const harnPath = config.get<string>("path", "harn");

  // --- LSP client ---
  const serverOptions: ServerOptions = {
    command: harnPath,
    args: ["lsp"],
  };

  const clientOptions: LanguageClientOptions = {
    documentSelector: [{ scheme: "file", language: "harn" }],
  };

  client = new LanguageClient(
    "harn-lsp",
    "Harn Language Server",
    serverOptions,
    clientOptions
  );

  client.start().catch((err) => {
    // LSP is optional — extension works for syntax highlighting without it
    console.warn("Harn LSP failed to start:", err);
  });

  // --- Run Pipeline command ---
  const runCommand = vscode.commands.registerCommand(
    "harn.runPipeline",
    async () => {
      const editor = vscode.window.activeTextEditor;
      if (!editor || editor.document.languageId !== "harn") {
        vscode.window.showWarningMessage("Open a .harn file first");
        return;
      }

      await editor.document.save();

      const terminal =
        vscode.window.terminals.find((t) => t.name === "Harn") ||
        vscode.window.createTerminal("Harn");

      terminal.show();
      terminal.sendText(`${harnPath} run "${editor.document.fileName}"`);
    }
  );

  // --- Format command ---
  const fmtCommand = vscode.commands.registerCommand(
    "harn.formatFile",
    async () => {
      const editor = vscode.window.activeTextEditor;
      if (!editor || editor.document.languageId !== "harn") {
        return;
      }

      await editor.document.save();

      const terminal =
        vscode.window.terminals.find((t) => t.name === "Harn") ||
        vscode.window.createTerminal("Harn");

      terminal.sendText(`${harnPath} fmt "${editor.document.fileName}"`);
    }
  );

  context.subscriptions.push(runCommand, fmtCommand);
}

export function deactivate(): Thenable<void> | undefined {
  return client?.stop();
}
