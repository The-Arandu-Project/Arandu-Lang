import * as assert from 'node:assert/strict';
import * as fs from 'node:fs';
import * as vscode from 'vscode';

const TIMEOUT_MS = 10_000;

interface AranduExtensionApi {
    getRuntimeState(): { state: string; observedCrashCount: number };
    testCrashServer(): Promise<void>;
}

export async function run(): Promise<void> {
    const serverPath = process.env.ARANDU_LSP_TEST_PATH;
    assert.ok(serverPath, 'ARANDU_LSP_TEST_PATH must point to the test server');
    assert.ok(fs.existsSync(serverPath), `arandu-lsp test binary missing: ${serverPath}`);

    const configuration = vscode.workspace.getConfiguration('arandu');
    await configuration.update('server.path', serverPath, vscode.ConfigurationTarget.Global);
    try {
        const workspace = vscode.workspace.workspaceFolders?.[0];
        assert.ok(workspace, 'Extension Host test requires a workspace');
        const uri = vscode.Uri.joinPath(workspace.uri, 'main.aru');
        const document = await vscode.workspace.openTextDocument(uri);
        await vscode.window.showTextDocument(document);

        const extension = vscode.extensions.getExtension('arandu.arandu-lang');
        assert.ok(extension, 'Arandu extension was not installed in the Extension Host');
        await extension.activate();
        const api = extension.exports as AranduExtensionApi;
        await poll(() => api.getRuntimeState().state === 'ready' ? true : undefined);

        const commands = await vscode.commands.getCommands(true);
        assert.ok(commands.includes('arandu.restartServer'));
        assert.ok(commands.includes('arandu.showServerLogs'));

        const diagnostics = await poll(() => {
            const current = vscode.languages.getDiagnostics(uri);
            return current.some(diagnostic => diagnosticCode(diagnostic) === 'N001') ? current : undefined;
        });
        assert.ok(diagnostics.some(diagnostic => diagnosticCode(diagnostic) === 'N001'));
        const unresolved = diagnostics.find(diagnostic => diagnosticCode(diagnostic) === 'N001');
        assert.ok(unresolved, 'Problems must retain the compiler diagnostic code');
        assert.equal(unresolved.source, 'arandu');
        assert.ok(unresolved.range.end.isAfter(unresolved.range.start));

        const completions = await poll(async () => {
            const result = await vscode.commands.executeCommand<vscode.CompletionList>(
                'vscode.executeCompletionItemProvider',
                uri,
                new vscode.Position(0, 4)
            );
            return result && result.items.length > 0 ? result : undefined;
        });
        assert.ok(completions.items.some(item => item.label === 'func'));

        const formatUri = vscode.Uri.joinPath(workspace.uri, 'format.aru');
        const formatDocument = await vscode.workspace.openTextDocument(formatUri);
        await vscode.window.showTextDocument(formatDocument);
        const formatOptions = { tabSize: 2, insertSpaces: false };
        const formatEdits = await poll(() =>
            vscode.commands.executeCommand<vscode.TextEdit[]>(
                'vscode.executeFormatDocumentProvider',
                formatUri,
                formatOptions
            ).then(edits => edits && edits.length > 0 ? edits : undefined)
        );
        assert.ok(formatEdits.length > 0);
        assert.ok(formatEdits.some(edit => edit.range.start.line === 1 && edit.range.end.line === 1));
        assert.ok(formatEdits.every(edit => edit.range.start.line > 0), 'unchanged header must not be replaced');
        const workspaceEdit = new vscode.WorkspaceEdit();
        workspaceEdit.set(formatUri, formatEdits);
        assert.equal(await vscode.workspace.applyEdit(workspaceEdit), true);
        assert.equal(formatDocument.getText(), 'func main(): int {\n    return 1\n}\n');
        const stableEdits = await vscode.commands.executeCommand<vscode.TextEdit[]>(
            'vscode.executeFormatDocumentProvider',
            formatUri,
            formatOptions
        );
        assert.ok(stableEdits === undefined || stableEdits.length === 0);

        await verifyHighlightingAcrossBuiltInThemes(uri);

        await vscode.commands.executeCommand('arandu.restartServer');
        const afterRestart = await poll(() =>
            vscode.commands.executeCommand<vscode.CompletionList>(
                'vscode.executeCompletionItemProvider',
                uri,
                new vscode.Position(0, 4)
            ).then(result => result && result.items.length > 0 ? result : undefined)
        );
        assert.ok(afterRestart.items.length > 0);

        const crashesBefore = api.getRuntimeState().observedCrashCount;
        await api.testCrashServer();
        await poll(() => api.getRuntimeState().observedCrashCount > crashesBefore ? true : undefined);
        await poll(() => api.getRuntimeState().state === 'ready' ? true : undefined);
        const afterCrashRecovery = await poll(() =>
            vscode.commands.executeCommand<vscode.CompletionList>(
                'vscode.executeCompletionItemProvider',
                uri,
                new vscode.Position(0, 4)
            ).then(result => result && result.items.length > 0 ? result : undefined)
        );
        assert.ok(afterCrashRecovery.items.length > 0, 'completion must recover after a real server crash');
    } finally {
        await configuration.update('server.path', undefined, vscode.ConfigurationTarget.Global);
        await vscode.commands.executeCommand('workbench.action.closeAllEditors');
    }
}

function diagnosticCode(diagnostic: vscode.Diagnostic): string | number | undefined {
    const code = diagnostic.code;
    return typeof code === 'object' ? code.value : code;
}

async function verifyHighlightingAcrossBuiltInThemes(uri: vscode.Uri): Promise<void> {
    const workbench = vscode.workspace.getConfiguration('workbench');
    const previousTheme = workbench.get<string>('colorTheme');
    const legend = await poll(() =>
        vscode.commands.executeCommand<vscode.SemanticTokensLegend>(
            'vscode.provideDocumentSemanticTokensLegend',
            uri
        )
    );
    assert.ok(legend.tokenTypes.includes('function'));
    assert.ok(legend.tokenTypes.includes('variable'));

    let baseline: number[] | undefined;
    const themes: ReadonlyArray<readonly [string, vscode.ColorThemeKind]> = [
        ['Default Dark+', vscode.ColorThemeKind.Dark],
        ['Default Light+', vscode.ColorThemeKind.Light],
        ['Default High Contrast', vscode.ColorThemeKind.HighContrast]
    ];
    try {
        for (const [theme, expectedKind] of themes) {
            await workbench.update('colorTheme', theme, vscode.ConfigurationTarget.Global);
            await poll(() => vscode.window.activeColorTheme.kind === expectedKind ? true : undefined);
            const tokens = await poll(() =>
                vscode.commands.executeCommand<vscode.SemanticTokens>(
                    'vscode.provideDocumentSemanticTokens',
                    uri
                ).then(value => value.data.length > 0 ? value : undefined)
            );
            assert.equal(tokens.data.length % 5, 0);
            const data = Array.from(tokens.data);
            if (baseline === undefined) {
                baseline = data;
            } else {
                assert.deepEqual(data, baseline, `semantic classification changed under ${theme}`);
            }
        }
    } finally {
        await workbench.update('colorTheme', previousTheme, vscode.ConfigurationTarget.Global);
    }
}

async function poll<T>(operation: () => T | undefined | PromiseLike<T | undefined>): Promise<T> {
    const deadline = Date.now() + TIMEOUT_MS;
    let lastError: unknown;
    while (Date.now() < deadline) {
        try {
            const result = await operation();
            if (result !== undefined) {
                return result;
            }
        } catch (error: unknown) {
            lastError = error;
        }
        await new Promise(resolve => setTimeout(resolve, 50));
    }
    const detail = lastError instanceof Error ? `: ${lastError.message}` : '';
    throw new Error(`Extension Host operation timed out${detail}`);
}
