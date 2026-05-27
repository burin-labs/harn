import * as vscode from "vscode";
import {
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
} from "vscode-languageclient/node";

let client: LanguageClient | undefined;

function runHarnTerminal(harnPath: string, args: string[]) {
  const terminal = vscode.window.createTerminal({
    name: `Harn: ${args[0]}`,
    shellPath: harnPath,
    shellArgs: args,
  });
  terminal.show();
}

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

interface HarnTaskDefinition extends vscode.TaskDefinition {
  type: "harn";
  command: "run" | "check" | "fmt" | "lint" | "test";
  file?: string;
  args?: string[];
  cwd?: string;
}

/**
 * Surfaces "Tasks: Run Task" entries for each harn subcommand so users
 * can wire `run`, `check`, `fmt`, `lint`, `test` into VS Code's task
 * runner (and chain them in `tasks.json`) without writing the shell
 * invocation by hand. The provider emits one task per command for the
 * currently active workspace folder; problem matchers attached in
 * package.json route diagnostics into the Problems panel.
 */
class HarnTaskProvider implements vscode.TaskProvider {
  private readonly harnPath: string;
  private cachedTasks: vscode.Task[] | undefined;

  constructor(harnPath: string) {
    this.harnPath = harnPath;
  }

  invalidate(): void {
    this.cachedTasks = undefined;
  }

  provideTasks(): vscode.ProviderResult<vscode.Task[]> {
    if (!this.cachedTasks) {
      this.cachedTasks = this.buildTasks();
    }
    return this.cachedTasks;
  }

  resolveTask(task: vscode.Task): vscode.ProviderResult<vscode.Task> {
    const def = task.definition as HarnTaskDefinition;
    if (def.type !== "harn" || !def.command) {
      return undefined;
    }
    const scope: vscode.WorkspaceFolder | vscode.TaskScope =
      task.scope ?? vscode.TaskScope.Workspace;
    return this.toTask(def, scope);
  }

  private buildTasks(): vscode.Task[] {
    const commands: HarnTaskDefinition["command"][] = [
      "run",
      "check",
      "fmt",
      "lint",
      "test",
    ];
    const scope: vscode.WorkspaceFolder | vscode.TaskScope =
      vscode.workspace.workspaceFolders?.[0] ?? vscode.TaskScope.Workspace;
    return commands.map((command) =>
      this.toTask({ type: "harn", command, file: "${file}", args: [] }, scope)
    );
  }

  private toTask(
    definition: HarnTaskDefinition,
    scope: vscode.WorkspaceFolder | vscode.TaskScope
  ): vscode.Task {
    const args: string[] = [definition.command];
    if (definition.file) {
      args.push(definition.file);
    }
    if (definition.args && definition.args.length > 0) {
      args.push(...definition.args);
    }
    const execution = new vscode.ProcessExecution(this.harnPath, args, {
      cwd: definition.cwd ?? "${workspaceFolder}",
    });
    const matchers = ["$harn", "$harn-lint"];
    const task = new vscode.Task(
      definition,
      scope,
      `harn ${definition.command}`,
      "harn",
      execution,
      matchers
    );
    task.group =
      definition.command === "test"
        ? vscode.TaskGroup.Test
        : definition.command === "check" || definition.command === "lint"
        ? vscode.TaskGroup.Build
        : undefined;
    return task;
  }
}

export function activate(context: vscode.ExtensionContext) {
  const config = vscode.workspace.getConfiguration("harn");
  const harnPath = config.get<string>("path", "harn");
  const lspPath = config.get<string>("lspPath", "harn-lsp");

  // --- LSP client ---
  const serverOptions: ServerOptions = {
    command: lspPath,
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

      runHarnTerminal(harnPath, ["run", editor.document.fileName]);
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

      runHarnTerminal(harnPath, ["fmt", editor.document.fileName]);
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

  const taskProvider = new HarnTaskProvider(harnPath);
  const taskProviderDisposable = vscode.tasks.registerTaskProvider(
    "harn",
    taskProvider
  );

  // Invalidate the task cache whenever the data that feeds it
  // changes — adding/removing a workspace folder shifts ${file} /
  // ${workspaceFolder} expansion, and changing `harn.path` swaps
  // the binary VS Code spawns for every task.
  const workspaceFolderWatcher =
    vscode.workspace.onDidChangeWorkspaceFolders(() => {
      taskProvider.invalidate();
    });
  const configWatcher = vscode.workspace.onDidChangeConfiguration(
    (event: vscode.ConfigurationChangeEvent) => {
      if (event.affectsConfiguration("harn.path")) {
        taskProvider.invalidate();
      }
    }
  );

  context.subscriptions.push(
    runCommand,
    fmtCommand,
    applyAllFixesCommand,
    debugConfigProvider,
    debugAdapterFactory,
    taskProviderDisposable,
    workspaceFolderWatcher,
    configWatcher
  );
}

export function deactivate(): Thenable<void> | undefined {
  return client?.stop();
}
