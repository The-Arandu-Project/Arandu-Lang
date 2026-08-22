import * as vscode from 'vscode';
import {
    LanguageClient,
    LanguageClientOptions,
    ServerOptions,
    State,
    TransportKind
} from 'vscode-languageclient/node';
import { discoverServer } from './serverDiscovery';

let client: LanguageClient | undefined;
let statusBarItem: vscode.StatusBarItem | undefined;
let traceOutputChannel: vscode.LogOutputChannel | undefined;
let clientStateSubscription: vscode.Disposable | undefined;
let lifecycle = Promise.resolve();
let deactivating = false;

export async function activate(context: vscode.ExtensionContext): Promise<void> {
    statusBarItem = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Right, 100);
    statusBarItem.command = 'arandu.showServerLogs';
    statusBarItem.accessibilityInformation = { label: 'Arandu language server status' };
    context.subscriptions.push(statusBarItem);

    traceOutputChannel = vscode.window.createOutputChannel('Arandu Language Server', { log: true });
    context.subscriptions.push(traceOutputChannel);

    const fileWatcher = vscode.workspace.createFileSystemWatcher('**/*.aru');
    context.subscriptions.push(fileWatcher);

    context.subscriptions.push(
        vscode.commands.registerCommand('arandu.showServerLogs', () => {
            traceOutputChannel?.show(true);
        }),
        vscode.commands.registerCommand('arandu.restartServer', () => restartLanguageServer(context, fileWatcher))
    );

    await restartLanguageServer(context, fileWatcher);
}

export async function deactivate(): Promise<void> {
    deactivating = true;
    await lifecycle;
    await stopClient();
}

function restartLanguageServer(
    context: vscode.ExtensionContext,
    fileWatcher: vscode.FileSystemWatcher
): Promise<void> {
    lifecycle = lifecycle.then(async () => {
        await stopClient();
        await startLanguageServer(context, fileWatcher);
    });
    return lifecycle;
}

async function startLanguageServer(
    context: vscode.ExtensionContext,
    fileWatcher: vscode.FileSystemWatcher
): Promise<void> {
    setStatus('starting');
    const configuration = vscode.workspace.getConfiguration('arandu');
    const result = discoverServer({
        configuredPath: configuration.get<string | null>('server.path'),
        workspaceRoots: (vscode.workspace.workspaceFolders ?? [])
            .filter(folder => folder.uri.scheme === 'file')
            .map(folder => folder.uri.fsPath),
        extensionPath: context.extensionPath
    });
    if (!result.resolution) {
        setStatus('missing', result.failure.message);
        traceOutputChannel?.error(result.failure.message);
        for (const candidate of result.failure.checked) {
            traceOutputChannel?.debug(`Checked ${candidate}`);
        }
        const action = await vscode.window.showErrorMessage(result.failure.message, 'Open Settings');
        if (action === 'Open Settings') {
            await vscode.commands.executeCommand('workbench.action.openSettings', 'arandu.server.path');
        }
        return;
    }

    traceOutputChannel?.info(
        `Starting ${result.resolution.command} (discovered via ${result.resolution.source})`
    );
    const serverOptions: ServerOptions = {
        run: { command: result.resolution.command, transport: TransportKind.stdio },
        debug: { command: result.resolution.command, transport: TransportKind.stdio }
    };
    const clientOptions: LanguageClientOptions = {
        documentSelector: [{ scheme: 'file', language: 'arandu' }],
        synchronize: { fileEvents: fileWatcher },
        traceOutputChannel
    };
    const nextClient = new LanguageClient(
        'aranduLanguageServer',
        'Arandu Language Server',
        serverOptions,
        clientOptions
    );
    client = nextClient;
    clientStateSubscription = nextClient.onDidChangeState(event => {
        if (event.newState === State.Starting) {
            setStatus('starting');
        } else if (event.newState === State.Running) {
            setStatus('running');
        } else if (!deactivating) {
            setStatus('stopped', 'Arandu Language Server stopped. Run “Arandu: Restart Language Server”.');
        }
    });

    try {
        await nextClient.start();
        setStatus('running');
    } catch (error: unknown) {
        if (client === nextClient) {
            client = undefined;
        }
        clientStateSubscription?.dispose();
        clientStateSubscription = undefined;
        const message = error instanceof Error ? error.message : String(error);
        traceOutputChannel?.error(`Failed to start Arandu Language Server: ${message}`);
        setStatus('stopped', `Arandu Language Server failed: ${message}`);
        const action = await vscode.window.showErrorMessage(
            `Failed to start Arandu Language Server: ${message}`,
            'Show Logs'
        );
        if (action === 'Show Logs') {
            traceOutputChannel?.show(true);
        }
    }
}

async function stopClient(): Promise<void> {
    clientStateSubscription?.dispose();
    clientStateSubscription = undefined;
    const activeClient = client;
    client = undefined;
    if (activeClient) {
        await activeClient.stop();
    }
}

function setStatus(
    state: 'starting' | 'running' | 'missing' | 'stopped',
    detail?: string
): void {
    if (!statusBarItem) {
        return;
    }
    switch (state) {
        case 'starting':
            statusBarItem.text = '$(sync~spin) Arandu';
            statusBarItem.tooltip = 'Arandu Language Server: Starting';
            break;
        case 'running':
            statusBarItem.text = '$(check) Arandu';
            statusBarItem.tooltip = 'Arandu Language Server: Running';
            break;
        case 'missing':
            statusBarItem.text = '$(warning) Arandu';
            statusBarItem.tooltip = detail ?? 'Arandu Language Server: Executable not found';
            break;
        case 'stopped':
            statusBarItem.text = '$(error) Arandu';
            statusBarItem.tooltip = detail ?? 'Arandu Language Server: Stopped';
            break;
    }
    statusBarItem.show();
}
