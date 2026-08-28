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
    const cliName = process.platform === 'win32' ? 'arandu_cli.exe' : 'arandu_cli';
    const aranduCliPath = process.env.ARANDU_CLI_TEST_PATH
        ? path.resolve(process.env.ARANDU_CLI_TEST_PATH)
        : path.join(repositoryRoot, 'target', 'debug', cliName);
    const userDataPath = path.join(extensionRoot, '.vscode-test', `installed-user-${process.pid}`);
    const extensionsPath = path.join(extensionRoot, '.vscode-test', `installed-extensions-${process.pid}`);

    if (!fs.existsSync(vsixPath)) {
        throw new Error(`packaged extension missing: ${vsixPath}`);
    }
    if (!fs.existsSync(serverPath)) {
        throw new Error(`Arandu LSP test binary missing: ${serverPath}`);
    }
    if (!fs.existsSync(aranduCliPath)) {
        throw new Error(`Arandu CLI test binary missing: ${aranduCliPath}`);
    }

    const vscodeExecutablePath = await downloadAndUnzipVSCode(VSCODE_VERSION);
    const [cliPath, ...cliArgs] = resolveCliArgsFromVSCodeExecutablePath(vscodeExecutablePath);
    fs.mkdirSync(extensionsPath, { recursive: true });
    const profileArgs = cliArgs.filter(argument =>
        !argument.startsWith('--user-data-dir=') && !argument.startsWith('--extensions-dir=')
    );
    const installArgs = [
        ...profileArgs,
        `--user-data-dir=${userDataPath}`,
        `--extensions-dir=${extensionsPath}`,
        '--install-extension',
        vsixPath,
        '--force'
    ];
    let installExecutable = cliPath;
    let installEnvironment = process.env;
    if (process.platform === 'win32') {
        const vscodeRoot = path.resolve(path.dirname(cliPath), '..');
        installExecutable = path.join(vscodeRoot, 'Code.exe');
        installArgs.unshift(path.join(vscodeRoot, 'resources', 'app', 'out', 'cli.js'));
        installEnvironment = { ...process.env, ELECTRON_RUN_AS_NODE: '1' };
    }
    const install = childProcess.spawnSync(
        installExecutable,
        installArgs,
        { encoding: 'utf8', stdio: 'inherit', env: installEnvironment }
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
                ARANDU_CLI_TEST_PATH: aranduCliPath,
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
