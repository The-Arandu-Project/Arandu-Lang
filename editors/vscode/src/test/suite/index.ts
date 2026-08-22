import * as assert from 'node:assert/strict';
import * as fs from 'node:fs';
import * as vscode from 'vscode';

const TIMEOUT_MS = 10_000;

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

        const commands = await vscode.commands.getCommands(true);
        assert.ok(commands.includes('arandu.restartServer'));
        assert.ok(commands.includes('arandu.showServerLogs'));

        const diagnostics = await poll(() => {
            const current = vscode.languages.getDiagnostics(uri);
            return current.length > 0 ? current : undefined;
        });
        assert.ok(diagnostics.some(diagnostic => diagnostic.code === 'N001'));

        const completions = await poll(async () => {
            const result = await vscode.commands.executeCommand<vscode.CompletionList>(
                'vscode.executeCompletionItemProvider',
                uri,
                new vscode.Position(0, 4)
            );
            return result && result.items.length > 0 ? result : undefined;
        });
        assert.ok(completions.items.some(item => item.label === 'func'));

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
    } finally {
        await configuration.update('server.path', undefined, vscode.ConfigurationTarget.Global);
        await vscode.commands.executeCommand('workbench.action.closeAllEditors');
    }
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
