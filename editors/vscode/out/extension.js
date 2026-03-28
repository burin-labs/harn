"use strict";
var __createBinding = (this && this.__createBinding) || (Object.create ? (function(o, m, k, k2) {
    if (k2 === undefined) k2 = k;
    var desc = Object.getOwnPropertyDescriptor(m, k);
    if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
      desc = { enumerable: true, get: function() { return m[k]; } };
    }
    Object.defineProperty(o, k2, desc);
}) : (function(o, m, k, k2) {
    if (k2 === undefined) k2 = k;
    o[k2] = m[k];
}));
var __setModuleDefault = (this && this.__setModuleDefault) || (Object.create ? (function(o, v) {
    Object.defineProperty(o, "default", { enumerable: true, value: v });
}) : function(o, v) {
    o["default"] = v;
});
var __importStar = (this && this.__importStar) || (function () {
    var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function (o) {
            var ar = [];
            for (var k in o) if (Object.prototype.hasOwnProperty.call(o, k)) ar[ar.length] = k;
            return ar;
        };
        return ownKeys(o);
    };
    return function (mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        __setModuleDefault(result, mod);
        return result;
    };
})();
Object.defineProperty(exports, "__esModule", { value: true });
exports.activate = activate;
exports.deactivate = deactivate;
const vscode = __importStar(require("vscode"));
const node_1 = require("vscode-languageclient/node");
let client;
function activate(context) {
    const config = vscode.workspace.getConfiguration("harn");
    const harnPath = config.get("path", "harn");
    // --- LSP client ---
    const serverOptions = {
        command: harnPath,
        args: ["lsp"],
    };
    const clientOptions = {
        documentSelector: [{ scheme: "file", language: "harn" }],
    };
    client = new node_1.LanguageClient("harn-lsp", "Harn Language Server", serverOptions, clientOptions);
    client.start().catch((err) => {
        // LSP is optional — extension works for syntax highlighting without it
        console.warn("Harn LSP failed to start:", err);
    });
    // --- Run Pipeline command ---
    const runCommand = vscode.commands.registerCommand("harn.runPipeline", async () => {
        const editor = vscode.window.activeTextEditor;
        if (!editor || editor.document.languageId !== "harn") {
            vscode.window.showWarningMessage("Open a .harn file first");
            return;
        }
        await editor.document.save();
        const terminal = vscode.window.terminals.find((t) => t.name === "Harn") ||
            vscode.window.createTerminal("Harn");
        terminal.show();
        terminal.sendText(`${harnPath} run "${editor.document.fileName}"`);
    });
    // --- Format command ---
    const fmtCommand = vscode.commands.registerCommand("harn.formatFile", async () => {
        const editor = vscode.window.activeTextEditor;
        if (!editor || editor.document.languageId !== "harn") {
            return;
        }
        await editor.document.save();
        const terminal = vscode.window.terminals.find((t) => t.name === "Harn") ||
            vscode.window.createTerminal("Harn");
        terminal.sendText(`${harnPath} fmt "${editor.document.fileName}"`);
    });
    context.subscriptions.push(runCommand, fmtCommand);
}
function deactivate() {
    return client?.stop();
}
//# sourceMappingURL=extension.js.map