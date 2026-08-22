import * as assert from 'node:assert/strict';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { afterEach, test } from 'node:test';
import { discoverServer } from '../../serverDiscovery';

const fixtures: string[] = [];

afterEach((): void => {
    for (const fixture of fixtures.splice(0)) {
        fs.rmSync(fixture, { recursive: true, force: true });
    }
});

void test('configured path must be absolute and executable', (): void => {
    const relative = discoverServer({
        configuredPath: 'arandu-lsp',
        workspaceRoots: [],
        extensionPath: '/extension'
    });
    assert.match(relative.failure?.message ?? '', /absolute path/);

    const root = fixture();
    const executable = makeExecutable(path.join(root, serverName()));
    const configured = discoverServer({
        configuredPath: executable,
        workspaceRoots: [],
        extensionPath: root
    });
    assert.equal(configured.resolution?.command, executable);
    assert.equal(configured.resolution?.source, 'configuration');
});

void test('release workspace binary wins over debug and PATH', (): void => {
    const root = fixture();
    const release = makeExecutable(path.join(root, 'target', 'release', serverName()));
    makeExecutable(path.join(root, 'target', 'debug', serverName()));
    const result = discoverServer({
        configuredPath: null,
        workspaceRoots: [root],
        extensionPath: fixture(),
        environment: { PATH: fixture() }
    });
    assert.equal(result.resolution?.command, release);
    assert.equal(result.resolution?.source, 'workspace');
});

void test('PATH fallback returns the resolved executable instead of a blind command', (): void => {
    const bin = fixture();
    const executable = makeExecutable(path.join(bin, serverName()));
    const result = discoverServer({
        configuredPath: null,
        workspaceRoots: [],
        extensionPath: fixture(),
        environment: { PATH: bin }
    });
    assert.equal(result.resolution?.command, executable);
    assert.equal(result.resolution?.source, 'path');
});

function fixture(): string {
    const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'arandu-vscode-test-'));
    fixtures.push(directory);
    return directory;
}

function makeExecutable(file: string): string {
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, 'fixture');
    if (process.platform !== 'win32') {
        fs.chmodSync(file, 0o755);
    }
    return file;
}

function serverName(): string {
    return process.platform === 'win32' ? 'arandu-lsp.exe' : 'arandu-lsp';
}
