import * as assert from 'node:assert/strict';
import * as fs from 'node:fs';
import * as path from 'node:path';
import { test } from 'node:test';

void test('TextMate grammar delegates colors to the active theme', () => {
    const grammarPath = path.resolve(__dirname, '..', '..', '..', 'syntaxes', 'arandu.tmLanguage.json');
    const grammar = fs.readFileSync(grammarPath, 'utf8');

    assert.doesNotMatch(grammar, /"(?:foreground|background)"\s*:/u);
    for (const scope of [
        'comment.line.double-slash.arandu',
        'string.quoted.double.arandu',
        'keyword.control.arandu',
        'constant.numeric.dec.arandu',
        'entity.name.function.arandu',
        'entity.name.tag.annotation.arandu',
        'entity.name.type.arandu',
        'variable.other.arandu'
    ]) {
        assert.match(grammar, new RegExp(scope.replaceAll('.', '\\.'), 'u'));
    }
});

void test('annotation semantic tokens use the TextMate annotation fallback', () => {
    const manifestPath = path.resolve(__dirname, '..', '..', '..', 'package.json');
    const manifest = JSON.parse(fs.readFileSync(manifestPath, 'utf8')) as {
        contributes?: {
            semanticTokenScopes?: Array<{
                scopes?: Record<string, string[]>;
            }>;
        };
    };
    const scopes = manifest.contributes?.semanticTokenScopes?.[0]?.scopes;
    assert.deepEqual(scopes?.decorator, ['entity.name.tag.annotation.arandu']);
});

void test('format on save is opt-in and Arandu owns its manual formatter', () => {
    const manifestPath = path.resolve(__dirname, '..', '..', '..', 'package.json');
    const manifest = JSON.parse(fs.readFileSync(manifestPath, 'utf8')) as {
        contributes?: {
            configurationDefaults?: Record<string, Record<string, unknown>>;
        };
    };
    const defaults = manifest.contributes?.configurationDefaults?.['[arandu]'];
    assert.equal(defaults?.['editor.defaultFormatter'], 'arandu.arandu-lang');
    assert.equal(defaults?.['editor.formatOnSave'], false);
});
