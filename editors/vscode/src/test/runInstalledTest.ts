import * as childProcess from 'node:child_process';
import * as fs from 'node:fs';
import * as path from 'node:path';
import {
    downloadAndUnzipVSCode,
    resolveCliArgsFromVSCodeExecutablePath,
    runTests
} from '@vscode/test-electron';

const VSCODE_VERSION = '1.92.0';

async function main(): Promise<void> {
    const extensionRoot = path.resolve(__dirname, '..', '..');
    const repositoryRoot = path.resolve(extensionRoot, '..', '..');
    const extensionTestsPath = path.resolve(__dirname, 'suite', 'index');
    const harnessPath = path.resolve(extensionRoot, 'src', 'test', 'installedHarness');
    const workspacePath = path.resolve(extensionRoot, 'src', 'test', 'fixture');
    const vsixPath = path.resolve(extensionRoot, '.vscode-test', 'arandu-lang.vsix');
    const serverName = process.platform === 'win32' ? 'arandu-lsp.exe' : 'arandu-lsp';
    const serverPath = process.env.ARANDU_LSP_TEST_PATH
        ? path.resolve(process.env.ARANDU_LSP_TEST_PATH)
        : path.join(repositoryRoot, 'target', 'debug', serverName);
    const userDataPath = path.join(extensionRoot, '.vscode-test', `installed-user-${process.pid}`);
    const extensionsPath = path.join(extensionRoot, '.vscode-test', `installed-extensions-${process.pid}`);

    if (!fs.existsSync(vsixPath)) {
        throw new Error(`packaged extension missing: ${vsixPath}`);
    }
    if (!fs.existsSync(serverPath)) {
        throw new Error(`Arandu LSP test binary missing: ${serverPath}`);
    }

    const vscodeExecutablePath = await downloadAndUnzipVSCode(VSCODE_VERSION);
    const [cliPath, ...cliArgs] = resolveCliArgsFromVSCodeExecutablePath(vscodeExecutablePath);
    fs.mkdirSync(extensionsPath, { recursive: true });
    const install = childProcess.spawnSync(
        cliPath,
        [
            ...cliArgs,
            `--user-data-dir=${userDataPath}`,
            `--extensions-dir=${extensionsPath}`,
            '--install-extension',
            vsixPath,
            '--force'
        ],
        { encoding: 'utf8', stdio: 'inherit', shell: process.platform === 'win32' }
    );
    if (install.status !== 0) {
        throw new Error(`VSIX installation failed with status ${String(install.status)}`);
    }

    try {
        await runTests({
            vscodeExecutablePath,
            extensionDevelopmentPath: harnessPath,
            extensionTestsPath,
            launchArgs: [
                workspacePath,
                `--user-data-dir=${userDataPath}`,
                `--extensions-dir=${extensionsPath}`
            ],
            extensionTestsEnv: {
                ARANDU_LSP_TEST_PATH: serverPath,
                ARANDU_LSP_TEST_ALLOW_CRASH: '1',
                ARANDU_EXPECT_INSTALLED_EXTENSION: '1'
            }
        });
    } finally {
        fs.rmSync(userDataPath, { recursive: true, force: true });
        fs.rmSync(extensionsPath, { recursive: true, force: true });
    }
}

void main().catch((error: unknown) => {
    console.error(error);
    process.exitCode = 1;
});
