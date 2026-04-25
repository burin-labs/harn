import * as vscode from "vscode";
import {
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
} from "vscode-languageclient/node";

let client: LanguageClient | undefined;

class HarnDebugConfigurationProvider
  implements vscode.DebugConfigurationProvider
{
  resolveDebugConfiguration(
    _folder: vscode.WorkspaceFolder | undefined,
    config: vscode.DebugConfiguration
  ): vscode.DebugConfiguration | null {
    if (!config.type && !config.request && !config.name) {
      config.type = "harn";
      config.request = "launch";
      config.name = "Debug Current Harn File";
      config.program = "${file}";
      config.cwd = "${workspaceFolder}";
    }

    if (!config.program) {
      const editor = vscode.window.activeTextEditor;
      if (!editor || editor.document.languageId !== "harn") {
        vscode.window.showErrorMessage("Open a .harn file to debug");
        return null;
      }
      config.program = editor.document.fileName;
    }

    if (!config.cwd) {
      config.cwd = "${workspaceFolder}";
    }

    return config;
  }
}

class HarnDebugAdapterFactory
  implements vscode.DebugAdapterDescriptorFactory
{
  createDebugAdapterDescriptor(
    _session: vscode.DebugSession
  ): vscode.ProviderResult<vscode.DebugAdapterDescriptor> {
    const config = vscode.workspace.getConfiguration("harn");
    const dapPath = config.get<string>("dapPath", "harn-dap");
    return new vscode.DebugAdapterExecutable(dapPath);
  }
}

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

  // --- Apply All Autofixes command ---
  // Triggers the LSP's bulk `source.fixAll.harn` code action. Same path
  // VS Code uses for `editor.codeActionsOnSave` — exposed as an explicit
  // command so users can run it on demand without configuring on-save.
  const applyAllFixesCommand = vscode.commands.registerCommand(
    "harn.applyAllAutofixes",
    async () => {
      const editor = vscode.window.activeTextEditor;
      if (!editor || editor.document.languageId !== "harn") {
        vscode.window.showWarningMessage("Open a .harn file first");
        return;
      }
      await vscode.commands.executeCommand(
        "editor.action.sourceAction",
        {
          kind: "source.fixAll.harn",
          apply: "first",
        }
      );
    }
  );

  const debugConfigProvider = vscode.debug.registerDebugConfigurationProvider(
    "harn",
    new HarnDebugConfigurationProvider()
  );
  const debugAdapterFactory = vscode.debug.registerDebugAdapterDescriptorFactory(
    "harn",
    new HarnDebugAdapterFactory()
  );

  context.subscriptions.push(
    runCommand,
    fmtCommand,
    applyAllFixesCommand,
    debugConfigProvider,
    debugAdapterFactory
  );
}

export function deactivate(): Thenable<void> | undefined {
  return client?.stop();
}
